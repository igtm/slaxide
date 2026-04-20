use std::{
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use slaxide_core::{AppSettings, TimelineItem};
use tokio::sync::{mpsc, oneshot};

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL
            );
            ",
        )?;
        self.migrate_timeline_items_table()?;
        Ok(())
    }

    fn migrate_timeline_items_table(&self) -> Result<()> {
        let mut statement = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'timeline_items'",
        )?;
        let has_timeline_items = statement.exists([])?;

        if !has_timeline_items {
            self.conn.execute_batch(
                "
                CREATE TABLE timeline_items (
                    workspace_key TEXT NOT NULL,
                    message_ts TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (workspace_key, message_ts)
                );
                CREATE INDEX IF NOT EXISTS idx_timeline_items_workspace_ts
                    ON timeline_items (workspace_key, message_ts DESC);
                ",
            )?;
            return Ok(());
        }

        let has_workspace_key = self
            .conn
            .prepare("PRAGMA table_info(timeline_items)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .any(|column| column == "workspace_key");

        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS timeline_items_v2 (
                workspace_key TEXT NOT NULL,
                message_ts TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                PRIMARY KEY (workspace_key, message_ts)
            );
            DELETE FROM timeline_items_v2;
            ",
        )?;
        if has_workspace_key {
            tx.execute(
                "
                INSERT OR REPLACE INTO timeline_items_v2 (workspace_key, message_ts, payload_json)
                SELECT workspace_key, message_ts, payload_json FROM timeline_items
                ",
                [],
            )?;
        } else {
            tx.execute(
                "
                INSERT OR REPLACE INTO timeline_items_v2 (workspace_key, message_ts, payload_json)
                SELECT 'default', message_ts, payload_json FROM timeline_items
                ",
                [],
            )?;
        }
        tx.execute_batch(
            "
            DROP TABLE timeline_items;
            ALTER TABLE timeline_items_v2 RENAME TO timeline_items;
            CREATE INDEX IF NOT EXISTS idx_timeline_items_workspace_ts
                ON timeline_items (workspace_key, message_ts DESC);
            ",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_timeline_items(
        &mut self,
        workspace_key: &str,
        items: &[TimelineItem],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM timeline_items WHERE workspace_key = ?1",
            [workspace_key],
        )?;

        {
            let mut statement = tx
                .prepare(
                    "INSERT INTO timeline_items (workspace_key, message_ts, payload_json) VALUES (?1, ?2, ?3)",
                )?;
            for item in items {
                statement.execute(params![
                    workspace_key,
                    item.message_ts,
                    serde_json::to_string(item)?
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn list_timeline_items(
        &self,
        workspace_key: &str,
        limit: usize,
    ) -> Result<Vec<TimelineItem>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT payload_json FROM timeline_items WHERE workspace_key = ?1 ORDER BY message_ts DESC LIMIT ?2",
            )?;
        let rows = statement.query_map(params![workspace_key, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let payloads = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        payloads
            .into_iter()
            .map(|payload| {
                serde_json::from_str::<TimelineItem>(&payload)
                    .context("failed to deserialize timeline item")
            })
            .collect()
    }

    pub fn prune_older_than(
        &mut self,
        workspace_key: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<usize> {
        let items = self
            .list_timeline_items(workspace_key, 10_000)?
            .into_iter()
            .filter(|item| item.last_activity_at >= cutoff)
            .collect::<Vec<_>>();
        let kept = items.len();
        self.replace_timeline_items(workspace_key, &items)?;
        Ok(kept)
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.conn.execute(
            "
            INSERT INTO app_settings (key, value_json)
            VALUES ('app', ?1)
            ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json
            ",
            [serde_json::to_string(settings)?],
        )?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Option<AppSettings>> {
        let mut statement = self
            .conn
            .prepare("SELECT value_json FROM app_settings WHERE key = 'app' LIMIT 1")?;
        let mut rows = statement.query([])?;
        if let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            return Ok(Some(serde_json::from_str(&payload)?));
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub struct StoreHandle {
    sender: mpsc::Sender<Command>,
}

enum Command {
    ReplaceTimelineItems {
        workspace_key: String,
        items: Vec<TimelineItem>,
        reply: oneshot::Sender<Result<()>>,
    },
    ListTimelineItems {
        workspace_key: String,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<TimelineItem>>>,
    },
    SaveSettings {
        settings: AppSettings,
        reply: oneshot::Sender<Result<()>>,
    },
    LoadSettings {
        reply: oneshot::Sender<Result<Option<AppSettings>>>,
    },
    PruneOlderThan {
        workspace_key: String,
        cutoff: DateTime<Utc>,
        reply: oneshot::Sender<Result<usize>>,
    },
}

impl StoreHandle {
    pub fn spawn(path: impl Into<PathBuf>) -> Result<Self> {
        let mut store = Store::open(path.into())?;
        let (sender, mut receiver) = mpsc::channel(32);

        thread::spawn(move || {
            while let Some(command) = receiver.blocking_recv() {
                match command {
                    Command::ReplaceTimelineItems {
                        workspace_key,
                        items,
                        reply,
                    } => {
                        let _ = reply.send(store.replace_timeline_items(&workspace_key, &items));
                    }
                    Command::ListTimelineItems {
                        workspace_key,
                        limit,
                        reply,
                    } => {
                        let _ = reply.send(store.list_timeline_items(&workspace_key, limit));
                    }
                    Command::SaveSettings { settings, reply } => {
                        let _ = reply.send(store.save_settings(&settings));
                    }
                    Command::LoadSettings { reply } => {
                        let _ = reply.send(store.load_settings());
                    }
                    Command::PruneOlderThan {
                        workspace_key,
                        cutoff,
                        reply,
                    } => {
                        let _ = reply.send(store.prune_older_than(&workspace_key, cutoff));
                    }
                }
            }
        });

        Ok(Self { sender })
    }

    pub async fn replace_timeline_items(
        &self,
        workspace_key: String,
        items: Vec<TimelineItem>,
    ) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::ReplaceTimelineItems {
                workspace_key,
                items,
                reply,
            })
            .await
            .map_err(|_| anyhow!("store actor stopped"))?;
        receive.await.map_err(|_| anyhow!("store actor stopped"))?
    }

    pub async fn list_timeline_items(
        &self,
        workspace_key: String,
        limit: usize,
    ) -> Result<Vec<TimelineItem>> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::ListTimelineItems {
                workspace_key,
                limit,
                reply,
            })
            .await
            .map_err(|_| anyhow!("store actor stopped"))?;
        receive.await.map_err(|_| anyhow!("store actor stopped"))?
    }

    pub async fn save_settings(&self, settings: AppSettings) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::SaveSettings { settings, reply })
            .await
            .map_err(|_| anyhow!("store actor stopped"))?;
        receive.await.map_err(|_| anyhow!("store actor stopped"))?
    }

    pub async fn load_settings(&self) -> Result<Option<AppSettings>> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::LoadSettings { reply })
            .await
            .map_err(|_| anyhow!("store actor stopped"))?;
        receive.await.map_err(|_| anyhow!("store actor stopped"))?
    }

    pub async fn prune_older_than(
        &self,
        workspace_key: String,
        cutoff: DateTime<Utc>,
    ) -> Result<usize> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Command::PruneOlderThan {
                workspace_key,
                cutoff,
                reply,
            })
            .await
            .map_err(|_| anyhow!("store actor stopped"))?;
        receive.await.map_err(|_| anyhow!("store actor stopped"))?
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use slaxide_core::sample::{sample_settings, sample_timeline};
    use tempfile::tempdir;

    use super::{Store, StoreHandle};

    #[test]
    fn store_round_trips_items_and_settings() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("slaxide.db");
        let mut store = Store::open(path).unwrap();
        let items = sample_timeline();
        let settings = sample_settings();
        let workspace_key = "sample-room";

        store.replace_timeline_items(workspace_key, &items).unwrap();
        store.save_settings(&settings).unwrap();

        assert_eq!(
            store.list_timeline_items(workspace_key, 10).unwrap().len(),
            items.len()
        );
        assert_eq!(store.load_settings().unwrap(), Some(settings));
        assert!(
            store
                .prune_older_than(workspace_key, Utc::now() - Duration::minutes(10))
                .unwrap()
                < items.len()
        );
    }

    #[tokio::test]
    async fn store_actor_proxies_commands() {
        let tempdir = tempdir().unwrap();
        let path = tempdir.path().join("actor.db");
        let handle = StoreHandle::spawn(path).unwrap();
        let items = sample_timeline();
        let workspace_key = "sample-room".to_string();

        handle
            .replace_timeline_items(workspace_key.clone(), items.clone())
            .await
            .unwrap();
        assert_eq!(
            handle
                .list_timeline_items(workspace_key, 10)
                .await
                .unwrap()
                .len(),
            items.len()
        );
    }
}

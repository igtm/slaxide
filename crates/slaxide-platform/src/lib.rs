use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use keyring::use_native_store;
use keyring_core::{Entry, Error as KeyringError};
use notify_rust::{Notification, Urgency};
use slaxide_core::NotificationAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRequest {
    pub summary: String,
    pub body: String,
    pub action: NotificationAction,
    pub icon: Option<String>,
    pub category: Option<String>,
}

#[async_trait]
pub trait NotificationBackend: Send + Sync {
    async fn send(&self, request: &NotificationRequest) -> Result<()>;
}

pub trait SecretStore: Send + Sync {
    fn set_secret(&self, account: &str, secret: &str) -> Result<()>;
    fn get_secret(&self, account: &str) -> Result<Option<String>>;
    fn delete_secret(&self, account: &str) -> Result<()>;
}

pub struct NotifyRustBackend;

#[async_trait]
impl NotificationBackend for NotifyRustBackend {
    async fn send(&self, request: &NotificationRequest) -> Result<()> {
        let mut notification = Notification::new();
        notification.summary(&request.summary).body(&request.body);

        if let Some(icon) = &request.icon {
            notification.icon(icon);
        }
        notification.urgency(map_urgency(request.action.clone()));
        if let Some(category) = &request.category {
            notification.appname(category);
        }
        notification.show()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct KeyringSecretStore {
    service_name: String,
}

impl KeyringSecretStore {
    pub fn new(service_name: impl Into<String>) -> Self {
        let _ = use_native_store(true);
        Self {
            service_name: service_name.into(),
        }
    }
}

impl SecretStore for KeyringSecretStore {
    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        Entry::new(&self.service_name, account)?.set_password(secret)?;
        Ok(())
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>> {
        let entry = Entry::new(&self.service_name, account)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(anyhow::Error::new(error)),
        }
    }

    fn delete_secret(&self, account: &str) -> Result<()> {
        let entry = Entry::new(&self.service_name, account)?;
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::Error::new(error)),
        }
    }
}

#[derive(Clone, Default)]
pub struct InMemorySecretStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
}

impl SecretStore for InMemorySecretStore {
    fn set_secret(&self, account: &str, secret: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("secret store mutex poisoned")
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .expect("secret store mutex poisoned")
            .get(account)
            .cloned())
    }

    fn delete_secret(&self, account: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("secret store mutex poisoned")
            .remove(account);
        Ok(())
    }
}

fn map_urgency(action: NotificationAction) -> Urgency {
    match action {
        NotificationAction::Notify => Urgency::Normal,
        NotificationAction::Silent => Urgency::Low,
        NotificationAction::Critical => Urgency::Critical,
    }
}

#[cfg(test)]
mod tests {
    use slaxide_core::NotificationAction;

    use super::{InMemorySecretStore, NotificationRequest, SecretStore, map_urgency};

    #[test]
    fn in_memory_secret_store_round_trips() {
        let store = InMemorySecretStore::default();
        store.set_secret("workspace", "token").unwrap();

        assert_eq!(
            store.get_secret("workspace").unwrap().as_deref(),
            Some("token")
        );

        store.delete_secret("workspace").unwrap();
        assert_eq!(store.get_secret("workspace").unwrap(), None);
    }

    #[test]
    fn urgency_mapping_matches_notification_action() {
        let request = NotificationRequest {
            summary: "Focus".into(),
            body: "Ship room".into(),
            action: NotificationAction::Critical,
            icon: None,
            category: None,
        };

        assert_eq!(map_urgency(request.action), notify_rust::Urgency::Critical);
    }
}

use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use keyring::use_native_store;
use keyring_core::{Entry, Error as KeyringError};
use notify_rust::{Hint, Notification, Urgency};
use slaxide_core::NotificationAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRequest {
    pub summary: String,
    pub body: String,
    pub action: NotificationAction,
    pub icon: Option<String>,
    pub category: Option<String>,
    pub sound_name: Option<String>,
    pub default_action_target: Option<String>,
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

#[derive(Clone, Default)]
pub struct NotifyRustBackend {
    activation_handler: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl NotifyRustBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_activation_handler(
        mut self,
        handler: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        self.activation_handler = Some(Arc::new(handler));
        self
    }
}

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
        if let Some(sound_name) = &request.sound_name {
            notification.hint(Hint::SuppressSound(true));
            notification.hint(Hint::SoundName(sound_name.clone()));
        }
        if request.default_action_target.is_some() {
            notification.action("default", "Open");
        }
        let handle = notification.show()?;
        if let Some(sound_name) = &request.sound_name {
            play_sound_name(sound_name);
        }
        if let (Some(target), Some(handler)) = (
            request.default_action_target.clone(),
            self.activation_handler.clone(),
        ) {
            handle.wait_for_action(move |action| {
                if action == "default" {
                    handler(target);
                }
            });
        }
        Ok(())
    }
}

fn play_sound_name(sound_name: &str) {
    let canberra_status = Command::new("canberra-gtk-play")
        .args(["-i", sound_name])
        .status();
    match canberra_status {
        Ok(status) if status.success() => return,
        Ok(status) => {
            eprintln!(
                "[slaxide] notification sound fallback: canberra-gtk-play exited with {status}"
            );
        }
        Err(error) => {
            eprintln!("[slaxide] notification sound fallback: canberra-gtk-play failed: {error}");
        }
    }

    let candidate_path = format!("/usr/share/sounds/freedesktop/stereo/{sound_name}.oga");
    if !Path::new(&candidate_path).exists() {
        eprintln!("[slaxide] notification sound file missing: {candidate_path}");
        return;
    }

    match Command::new("paplay").arg(&candidate_path).status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("[slaxide] notification sound playback failed: paplay exited with {status}");
        }
        Err(error) => {
            eprintln!("[slaxide] notification sound playback failed: {error}");
        }
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
            sound_name: Some("message-new-instant".into()),
            default_action_target: Some("1".into()),
        };

        assert_eq!(map_urgency(request.action), notify_rust::Urgency::Critical);
    }
}

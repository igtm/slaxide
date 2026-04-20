use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use slaxide_platform::SecretStore;
use slaxide_slack::{AuthedUser, OAuthAccessResponse, SlackClient, SlackOAuthConfig};
use tokio::runtime::Runtime;
use url::Url;
use uuid::Uuid;

pub const CLIENT_ID_ENV: &str = "SLAXIDE_SLACK_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "SLAXIDE_SLACK_CLIENT_SECRET";
pub const REDIRECT_URI_ENV: &str = "SLAXIDE_SLACK_REDIRECT_URI";
pub const USER_SCOPES_ENV: &str = "SLAXIDE_SLACK_USER_SCOPES";
pub const KEYRING_ACCOUNT: &str = "slack.oauth.default";
pub const WORKSPACE_ACCOUNT_PREFIX: &str = "slack.oauth.workspace.";
pub const DEFAULT_WORKSPACE_KEY: &str = "default";
pub const DEFAULT_REDIRECT_URI: &str = "https://127.0.0.1/slack/callback";
pub const DEFAULT_USER_SCOPES: &[&str] = &[
    "channels:history",
    "channels:read",
    "channels:write",
    "channels:write.invites",
    "groups:history",
    "groups:read",
    "groups:write",
    "groups:write.invites",
    "chat:write",
    "files:read",
    "files:write",
    "reactions:read",
    "reactions:write",
    "users:read",
];
const REQUIRED_USER_SCOPES: &[&str] = &[
    "channels:history",
    "channels:read",
    "channels:write",
    "channels:write.invites",
    "groups:history",
    "groups:read",
    "groups:write",
    "groups:write.invites",
    "chat:write",
    "files:read",
    "files:write",
    "reactions:read",
    "reactions:write",
    "users:read",
];

#[derive(Clone, Debug)]
pub struct SlackOAuthEnvironment {
    client_id: String,
    client_secret: String,
    redirect_uri: Url,
    user_scopes: Vec<String>,
}

impl SlackOAuthEnvironment {
    pub fn from_env() -> Result<Self> {
        let client_id = required_env(CLIENT_ID_ENV)?;
        let client_secret = required_env(CLIENT_SECRET_ENV)?;
        let redirect_uri_raw =
            std::env::var(REDIRECT_URI_ENV).unwrap_or_else(|_| DEFAULT_REDIRECT_URI.to_string());
        let redirect_uri = Url::parse(&redirect_uri_raw)
            .with_context(|| format!("invalid `{REDIRECT_URI_ENV}` value `{redirect_uri_raw}`"))?;
        if redirect_uri.scheme() != "https" {
            bail!("`{REDIRECT_URI_ENV}` must be an HTTPS URL configured in Slack OAuth settings");
        }
        let user_scopes = match std::env::var(USER_SCOPES_ENV) {
            Ok(value) => ensure_required_user_scopes(parse_scope_list(&value)?),
            Err(_) => DEFAULT_USER_SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
        };

        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
            user_scopes,
        })
    }

    pub fn user_scopes(&self) -> &[String] {
        &self.user_scopes
    }

    pub fn user_scope_refs(&self) -> Vec<&str> {
        self.user_scopes.iter().map(String::as_str).collect()
    }

    pub fn redirect_uri(&self) -> &Url {
        &self.redirect_uri
    }

    pub fn client(&self) -> Result<SlackClient> {
        let mut config = SlackOAuthConfig::new(&self.client_id, &self.client_secret)?;
        config.redirect_uri = self.redirect_uri.clone();
        Ok(SlackClient::new(config))
    }
}

fn required_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("set `{key}` to enable Slack OAuth"))
}

fn parse_scope_list(raw: &str) -> Result<Vec<String>> {
    let scopes = raw
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if scopes.is_empty() {
        bail!("`{USER_SCOPES_ENV}` must contain at least one scope");
    }
    Ok(scopes)
}

fn ensure_required_user_scopes(mut scopes: Vec<String>) -> Vec<String> {
    for required in REQUIRED_USER_SCOPES {
        if !scopes.iter().any(|scope| scope == required) {
            scopes.push((*required).to_string());
        }
    }
    scopes
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSlackSession {
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub app_id: Option<String>,
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub bot_user_id: Option<String>,
    pub bot_access_token: Option<String>,
    pub bot_refresh_token: Option<String>,
    pub bot_expires_at: Option<DateTime<Utc>>,
    pub user_id: Option<String>,
    pub user_access_token: Option<String>,
    pub user_refresh_token: Option<String>,
    pub user_expires_at: Option<DateTime<Utc>>,
}

impl StoredSlackSession {
    pub fn from_oauth_response(response: OAuthAccessResponse) -> Result<Self> {
        let now = Utc::now();
        let mut session = Self {
            installed_at: now,
            updated_at: now,
            app_id: None,
            team_id: None,
            team_name: None,
            bot_user_id: None,
            bot_access_token: None,
            bot_refresh_token: None,
            bot_expires_at: None,
            user_id: None,
            user_access_token: None,
            user_refresh_token: None,
            user_expires_at: None,
        };
        session.apply_initial_response(response)?;
        Ok(session)
    }

    pub fn summary(&self) -> String {
        let workspace = self
            .team_name
            .as_ref()
            .or(self.team_id.as_ref())
            .cloned()
            .unwrap_or_else(|| "workspace".to_string());
        let principal = self
            .user_id
            .as_deref()
            .map(|user| format!("user {user}"))
            .or_else(|| {
                self.bot_user_id
                    .as_deref()
                    .map(|user| format!("bot {user}"))
            })
            .unwrap_or_else(|| "stored credentials".to_string());
        if let Some(app_id) = self.app_id.as_deref() {
            format!("Connected to {workspace} as {principal} via app {app_id}.")
        } else {
            format!("Connected to {workspace} as {principal}.")
        }
    }

    pub fn needs_refresh(&self) -> bool {
        self.user_token_needs_refresh() || self.bot_token_needs_refresh()
    }

    pub fn user_token_needs_refresh(&self) -> bool {
        token_needs_refresh(self.user_expires_at, self.user_refresh_token.as_deref())
    }

    pub fn bot_token_needs_refresh(&self) -> bool {
        token_needs_refresh(self.bot_expires_at, self.bot_refresh_token.as_deref())
    }

    pub fn apply_user_refresh_response(&mut self, response: OAuthAccessResponse) -> Result<()> {
        let user = user_bundle_from_response(&response)
            .ok_or_else(|| anyhow!("refresh response did not include a rotated user token"))?;
        self.user_id = user.id;
        self.user_access_token = Some(user.access_token);
        self.user_refresh_token = user.refresh_token;
        self.user_expires_at = user.expires_at;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn apply_bot_refresh_response(&mut self, response: OAuthAccessResponse) -> Result<()> {
        let access_token = response
            .access_token
            .ok_or_else(|| anyhow!("refresh response did not include a rotated bot token"))?;
        self.bot_access_token = Some(access_token);
        self.bot_refresh_token = response.refresh_token;
        self.bot_expires_at = expires_at(response.expires_in);
        self.bot_user_id = response.bot_user_id;
        self.updated_at = Utc::now();
        Ok(())
    }

    fn apply_initial_response(&mut self, response: OAuthAccessResponse) -> Result<()> {
        self.app_id = response.app_id.clone();

        if let Some(team) = response.team.as_ref() {
            self.team_id = team.id.clone();
            self.team_name = team.name.clone();
        }

        if let Some(access_token) = response.access_token.clone() {
            self.bot_access_token = Some(access_token);
            self.bot_refresh_token = response.refresh_token.clone();
            self.bot_expires_at = expires_at(response.expires_in);
            self.bot_user_id = response.bot_user_id.clone();
        }

        if let Some(user) = user_bundle_from_response(&response) {
            self.user_id = user.id;
            self.user_access_token = Some(user.access_token);
            self.user_refresh_token = user.refresh_token;
            self.user_expires_at = user.expires_at;
        }

        if self.user_access_token.is_none() && self.bot_access_token.is_none() {
            bail!("OAuth response did not contain any usable token");
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
struct UserBundle {
    id: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

fn user_bundle_from_response(response: &OAuthAccessResponse) -> Option<UserBundle> {
    if let Some(AuthedUser {
        id,
        access_token: Some(access_token),
        refresh_token,
        expires_in,
        ..
    }) = response.authed_user.as_ref()
    {
        return Some(UserBundle {
            id: id.clone(),
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            expires_at: expires_at(*expires_in),
        });
    }

    if response.token_type.as_deref() == Some("user") {
        return response
            .access_token
            .as_ref()
            .map(|access_token| UserBundle {
                id: None,
                access_token: access_token.clone(),
                refresh_token: response.refresh_token.clone(),
                expires_at: expires_at(response.expires_in),
            });
    }

    None
}

fn expires_at(expires_in: Option<i64>) -> Option<DateTime<Utc>> {
    expires_in.map(|seconds| Utc::now() + chrono::Duration::seconds(seconds))
}

fn token_needs_refresh(expires_at: Option<DateTime<Utc>>, refresh_token: Option<&str>) -> bool {
    let Some(refresh_token) = refresh_token else {
        return false;
    };
    if refresh_token.is_empty() {
        return false;
    }

    let Some(expires_at) = expires_at else {
        return false;
    };

    expires_at <= Utc::now() + chrono::Duration::minutes(5)
}

#[derive(Clone, Debug)]
pub struct PendingSlackLogin {
    state: String,
    redirect_uri: Url,
}

impl PendingSlackLogin {
    pub fn redirect_uri(&self) -> &Url {
        &self.redirect_uri
    }
}

#[derive(Clone)]
pub struct SlackAuthController<S: SecretStore + Clone> {
    secret_store: S,
}

impl<S: SecretStore + Clone> SlackAuthController<S> {
    pub fn new(secret_store: S) -> Self {
        Self { secret_store }
    }

    pub fn initial_status_for(&self, workspace_key: &str) -> SlackAuthStatus {
        match self.load_and_refresh_session_for(workspace_key) {
            Ok(Some(session)) => SlackAuthStatus::Connected(session),
            Ok(None) => match SlackOAuthEnvironment::from_env() {
                Ok(env) => SlackAuthStatus::Disconnected {
                    scopes: env.user_scopes().to_vec(),
                    redirect_uri: env.redirect_uri().to_string(),
                },
                Err(error) => SlackAuthStatus::MissingConfig(error.to_string()),
            },
            Err(error) => SlackAuthStatus::Error(error.to_string()),
        }
    }

    pub fn load_and_refresh_session_for(
        &self,
        workspace_key: &str,
    ) -> Result<Option<StoredSlackSession>> {
        let Some(session) = self.load_session_for(workspace_key)? else {
            return Ok(None);
        };

        let environment = match SlackOAuthEnvironment::from_env() {
            Ok(environment) => environment,
            Err(_) => return Ok(Some(session)),
        };

        if session.needs_refresh() {
            let refreshed = self.refresh_session(&environment, session)?;
            self.save_session_for(workspace_key, &refreshed)?;
            Ok(Some(refreshed))
        } else {
            Ok(Some(session))
        }
    }

    pub fn begin_login(&self) -> Result<PendingSlackLogin> {
        let environment = SlackOAuthEnvironment::from_env()?;
        let client = environment.client()?;
        let state = Uuid::new_v4().simple().to_string();
        let authorize_url = client.authorize_url(&environment.user_scope_refs(), &state)?;
        open_browser(authorize_url.as_str())?;
        Ok(PendingSlackLogin {
            state,
            redirect_uri: environment.redirect_uri().clone(),
        })
    }

    pub fn finish_login(
        &self,
        workspace_key: &str,
        pending: &PendingSlackLogin,
        callback_input: &str,
    ) -> Result<StoredSlackSession> {
        let environment = SlackOAuthEnvironment::from_env()?;
        let callback =
            parse_callback_input(callback_input, pending.redirect_uri(), &pending.state)?;
        let runtime = Runtime::new().context("failed to create runtime for Slack OAuth")?;
        let response = runtime
            .block_on(environment.client()?.exchange_code(&callback.code))
            .context("failed to exchange Slack OAuth code")?;
        let session = StoredSlackSession::from_oauth_response(response)?;
        self.save_session_for(workspace_key, &session)?;
        Ok(session)
    }

    pub fn clear_session_for(&self, workspace_key: &str) -> Result<()> {
        self.secret_store
            .delete_secret(&keyring_account_for(workspace_key))
    }

    fn refresh_session(
        &self,
        environment: &SlackOAuthEnvironment,
        mut session: StoredSlackSession,
    ) -> Result<StoredSlackSession> {
        let runtime = Runtime::new().context("failed to create runtime for token refresh")?;
        let client = environment.client()?;

        if session.user_token_needs_refresh() {
            let refresh_token = session
                .user_refresh_token
                .clone()
                .ok_or_else(|| anyhow!("user token is expiring but no refresh token is stored"))?;
            let response = runtime
                .block_on(client.refresh_token(&refresh_token))
                .context("failed to refresh Slack user token")?;
            session.apply_user_refresh_response(response)?;
        }

        if session.bot_token_needs_refresh() {
            let refresh_token = session
                .bot_refresh_token
                .clone()
                .ok_or_else(|| anyhow!("bot token is expiring but no refresh token is stored"))?;
            let response = runtime
                .block_on(client.refresh_token(&refresh_token))
                .context("failed to refresh Slack bot token")?;
            session.apply_bot_refresh_response(response)?;
        }

        Ok(session)
    }

    fn save_session_for(&self, workspace_key: &str, session: &StoredSlackSession) -> Result<()> {
        let payload = serde_json::to_string(session).context("failed to serialize Slack auth")?;
        self.secret_store
            .set_secret(&keyring_account_for(workspace_key), &payload)
            .context("failed to write Slack auth to secret store")
    }

    fn load_session_for(&self, workspace_key: &str) -> Result<Option<StoredSlackSession>> {
        let Some(payload) = self
            .secret_store
            .get_secret(&keyring_account_for(workspace_key))
            .context("failed to read Slack auth from secret store")?
        else {
            return Ok(None);
        };

        let session =
            serde_json::from_str(&payload).context("failed to parse stored Slack auth")?;
        Ok(Some(session))
    }
}

pub fn keyring_account_for(workspace_key: &str) -> String {
    if workspace_key == DEFAULT_WORKSPACE_KEY {
        KEYRING_ACCOUNT.to_string()
    } else {
        format!("{WORKSPACE_ACCOUNT_PREFIX}{workspace_key}")
    }
}

#[derive(Clone, Debug)]
pub enum SlackAuthStatus {
    MissingConfig(String),
    Disconnected {
        scopes: Vec<String>,
        redirect_uri: String,
    },
    Connecting(String),
    Connected(StoredSlackSession),
    Error(String),
}

impl SlackAuthStatus {
    pub fn summary(&self) -> String {
        match self {
            Self::MissingConfig(message) => message.clone(),
            Self::Disconnected {
                scopes,
                redirect_uri,
            } => format!(
                "Ready to connect. Redirect URL: {redirect_uri}. Requested user scopes: {}",
                scopes.join(", ")
            ),
            Self::Connecting(message) => message.clone(),
            Self::Connected(session) => session.summary(),
            Self::Error(message) => message.clone(),
        }
    }

    pub fn button_label(&self) -> &'static str {
        match self {
            Self::Connected(_) => "Reconnect Slack",
            _ => "Connect Slack",
        }
    }

    pub fn can_start_login(&self) -> bool {
        matches!(
            self,
            Self::Disconnected { .. } | Self::Connected(_) | Self::Error(_)
        )
    }

    pub fn empty_state(&self) -> (&'static str, String) {
        match self {
            Self::MissingConfig(message) => ("Slack OAuth is not configured", message.clone()),
            Self::Disconnected { redirect_uri, .. } => (
                "Connect Slack to start building the cache",
                format!(
                    "Set Slack Redirect URL to {redirect_uri}, then use Connect Slack and paste the redirected URL back into Slaxide."
                ),
            ),
            Self::Connecting(message) => ("Waiting for Slack authorization", message.clone()),
            Self::Connected(_) => (
                "No cached timeline yet",
                "Slack OAuth is connected. History ingest and live sync are the next steps."
                    .to_string(),
            ),
            Self::Error(message) => ("Slack OAuth failed", message.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CallbackCode {
    code: String,
}

fn parse_callback_input(
    raw_input: &str,
    redirect_uri: &Url,
    expected_state: &str,
) -> Result<CallbackCode> {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        bail!("paste the full redirected Slack URL");
    }

    let callback_url = match Url::parse(trimmed) {
        Ok(url) => url,
        Err(_) if trimmed.starts_with('/') => redirect_uri
            .join(trimmed)
            .with_context(|| format!("invalid callback URL path `{trimmed}`"))?,
        Err(_) => bail!("paste the full redirected Slack URL, not just the code"),
    };

    if callback_url.scheme() != redirect_uri.scheme() {
        bail!(
            "OAuth callback used unexpected scheme `{}`",
            callback_url.scheme()
        );
    }

    if is_slack_authorize_url(&callback_url) {
        bail!(
            "you pasted the Slack authorize page URL; click Allow first, then paste the redirected URL that starts with `{}`",
            redirect_uri
        );
    }

    if callback_url.host_str() != redirect_uri.host_str()
        || callback_url.port_or_known_default() != redirect_uri.port_or_known_default()
    {
        bail!(
            "OAuth callback host did not match the configured redirect URL `{}`",
            redirect_uri
        );
    }

    if callback_url.path() != redirect_uri.path() {
        bail!(
            "OAuth callback hit unexpected path `{}`",
            callback_url.path()
        );
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;

    for (key, value) in callback_url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        bail!("Slack returned OAuth error `{error}`");
    }

    let returned_state = state.ok_or_else(|| anyhow!("OAuth callback was missing `state`"))?;
    if returned_state != expected_state {
        bail!("OAuth callback `state` did not match the login attempt");
    }

    let code = code.ok_or_else(|| anyhow!("OAuth callback was missing `code`"))?;
    Ok(CallbackCode { code })
}

fn is_slack_authorize_url(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.ends_with(".slack.com") || host == "slack.com")
        && matches!(url.path(), "/oauth" | "/oauth/v2/authorize")
}

fn open_browser(url: &str) -> Result<()> {
    if spawn_browser_command("xdg-open", &[url])? || spawn_browser_command("gio", &["open", url])? {
        Ok(())
    } else {
        bail!("could not launch a browser; tried `xdg-open` and `gio open`")
    }
}

fn spawn_browser_command(program: &str, args: &[&str]) -> Result<bool> {
    match Command::new(program).args(args).spawn() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to start browser command `{program}`"))
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use slaxide_slack::{AuthedUser, OAuthAccessResponse};
    use url::Url;

    use super::{
        CLIENT_ID_ENV, CLIENT_SECRET_ENV, DEFAULT_REDIRECT_URI, REDIRECT_URI_ENV,
        SlackOAuthEnvironment, StoredSlackSession, USER_SCOPES_ENV, parse_callback_input,
        token_needs_refresh,
    };

    #[test]
    fn stored_session_prefers_authed_user_token() {
        let session = StoredSlackSession::from_oauth_response(OAuthAccessResponse {
            access_token: Some("xoxb-bot".into()),
            token_type: Some("bot".into()),
            scope: None,
            refresh_token: Some("bot-refresh".into()),
            expires_in: Some(7200),
            bot_user_id: Some("B1".into()),
            app_id: None,
            team: None,
            enterprise: None,
            is_enterprise_install: None,
            authed_user: Some(AuthedUser {
                id: Some("U1".into()),
                access_token: Some("xoxp-user".into()),
                scope: None,
                refresh_token: Some("user-refresh".into()),
                expires_in: Some(3600),
                token_type: Some("user".into()),
            }),
        })
        .unwrap();

        assert_eq!(session.user_id.as_deref(), Some("U1"));
        assert_eq!(session.user_access_token.as_deref(), Some("xoxp-user"));
        assert_eq!(session.bot_access_token.as_deref(), Some("xoxb-bot"));
    }

    #[test]
    fn callback_parser_extracts_code_and_validates_state() {
        let redirect_uri = Url::parse("https://127.0.0.1/slack/callback").unwrap();
        let callback = parse_callback_input(
            "https://127.0.0.1/slack/callback?code=abc123&state=expected",
            &redirect_uri,
            "expected",
        )
        .unwrap();

        assert_eq!(callback.code, "abc123");
    }

    #[test]
    fn refresh_window_starts_before_expiry() {
        let refresh_token = Some("refresh-token");
        let expires_at = Some(chrono::Utc::now() + Duration::minutes(4));

        assert!(token_needs_refresh(expires_at, refresh_token));
    }

    #[test]
    fn callback_parser_rejects_slack_authorize_page_url() {
        let redirect_uri = Url::parse("https://127.0.0.1/slack/callback").unwrap();
        let error = parse_callback_input(
            "https://iguchi-labo.slack.com/oauth?client_id=123&redirect_uri=https%3A%2F%2F127.0.0.1%2Fslack%2Fcallback&state=expected",
            &redirect_uri,
            "expected",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("click Allow first"));
    }

    #[test]
    fn oauth_environment_requires_https_redirect_uri() {
        let _guard = EnvGuard::set(&[
            (CLIENT_ID_ENV, Some("client-id")),
            (CLIENT_SECRET_ENV, Some("client-secret")),
            (REDIRECT_URI_ENV, Some("http://127.0.0.1/slack/callback")),
        ]);

        let error = SlackOAuthEnvironment::from_env().unwrap_err().to_string();

        assert!(error.contains("HTTPS URL"));
    }

    #[test]
    fn oauth_environment_uses_default_redirect_uri() {
        let _guard = EnvGuard::set(&[
            (CLIENT_ID_ENV, Some("client-id")),
            (CLIENT_SECRET_ENV, Some("client-secret")),
            (REDIRECT_URI_ENV, None),
        ]);

        let environment = SlackOAuthEnvironment::from_env().unwrap();

        assert_eq!(environment.redirect_uri().as_str(), DEFAULT_REDIRECT_URI);
    }

    #[test]
    fn oauth_environment_adds_required_reaction_scope() {
        let _guard = EnvGuard::set(&[
            (CLIENT_ID_ENV, Some("client-id")),
            (CLIENT_SECRET_ENV, Some("client-secret")),
            (REDIRECT_URI_ENV, None),
            (USER_SCOPES_ENV, Some("channels:history,chat:write")),
        ]);

        let environment = SlackOAuthEnvironment::from_env().unwrap();

        assert!(
            environment
                .user_scopes()
                .iter()
                .any(|scope| scope == "reactions:read")
        );
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(entries: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().expect("env test lock poisoned");
            let previous = entries
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for (key, value) in entries {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }
}

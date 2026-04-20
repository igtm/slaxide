use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;

#[derive(Clone, Debug)]
pub struct SlackOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: Url,
    pub authorize_base: Url,
    pub api_base: Url,
}

impl SlackOAuthConfig {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Result<Self, SlackError> {
        Ok(Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: Url::parse("https://127.0.0.1/slack/callback")?,
            authorize_base: Url::parse("https://slack.com/oauth/v2/authorize")?,
            api_base: Url::parse("https://slack.com/api/")?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SlackClient {
    http: Client,
    config: SlackOAuthConfig,
}

impl SlackClient {
    pub fn api_only() -> Result<Self, SlackError> {
        Ok(Self::new(SlackOAuthConfig {
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: Url::parse("https://127.0.0.1/slack/callback")?,
            authorize_base: Url::parse("https://slack.com/oauth/v2/authorize")?,
            api_base: Url::parse("https://slack.com/api/")?,
        }))
    }

    pub fn new(config: SlackOAuthConfig) -> Self {
        Self {
            http: Client::new(),
            config,
        }
    }

    pub fn authorize_url(&self, user_scopes: &[&str], state: &str) -> Result<Url, SlackError> {
        Url::parse_with_params(
            self.config.authorize_base.as_str(),
            &[
                ("client_id", self.config.client_id.as_str()),
                ("redirect_uri", self.config.redirect_uri.as_str()),
                ("user_scope", &user_scopes.join(",")),
                ("state", state),
            ],
        )
        .map_err(Into::into)
    }

    pub async fn exchange_code(&self, code: &str) -> Result<OAuthAccessResponse, SlackError> {
        self.form_request(
            "oauth.v2.access",
            None,
            &[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.config.redirect_uri.as_str()),
            ],
        )
        .await
    }

    pub async fn refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthAccessResponse, SlackError> {
        self.form_request(
            "oauth.v2.access",
            None,
            &[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
    }

    pub async fn open_socket_connection(
        &self,
        app_token: &str,
    ) -> Result<SocketConnectionResponse, SlackError> {
        self.form_request("apps.connections.open", Some(app_token), &[])
            .await
    }

    pub async fn list_conversations(
        &self,
        user_token: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ConversationsListResponse, SlackError> {
        let mut query = vec![
            ("types", "public_channel,private_channel".to_string()),
            ("exclude_archived", "true".to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_string()));
        }

        self.get_owned_request("conversations.list", user_token, query)
            .await
    }

    pub async fn conversations_history(
        &self,
        user_token: &str,
        channel: &str,
        oldest: Option<&str>,
        limit: usize,
    ) -> Result<ConversationsHistoryResponse, SlackError> {
        let mut query = vec![
            ("channel", channel.to_string()),
            ("inclusive", "true".to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(oldest) = oldest.filter(|oldest| !oldest.is_empty()) {
            query.push(("oldest", oldest.to_string()));
        }

        self.get_owned_request("conversations.history", user_token, query)
            .await
    }

    pub async fn users_list(
        &self,
        user_token: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<UsersListResponse, SlackError> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_string()));
        }

        self.get_owned_request("users.list", user_token, query)
            .await
    }

    pub async fn post_thread_reply(
        &self,
        user_token: &str,
        channel: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<ChatPostMessageResponse, SlackError> {
        self.json_request(
            "chat.postMessage",
            user_token,
            &serde_json::json!({
                "channel": channel,
                "thread_ts": thread_ts,
                "text": text,
                "unfurl_links": false,
                "unfurl_media": false,
            }),
        )
        .await
    }

    pub async fn post_message(
        &self,
        user_token: &str,
        channel: &str,
        text: &str,
    ) -> Result<ChatPostMessageResponse, SlackError> {
        self.json_request(
            "chat.postMessage",
            user_token,
            &serde_json::json!({
                "channel": channel,
                "text": text,
                "unfurl_links": false,
                "unfurl_media": false,
            }),
        )
        .await
    }

    pub async fn update_message(
        &self,
        user_token: &str,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<ChatUpdateResponse, SlackError> {
        self.json_request(
            "chat.update",
            user_token,
            &serde_json::json!({
                "channel": channel,
                "ts": ts,
                "text": text,
                "unfurl_links": false,
                "unfurl_media": false,
            }),
        )
        .await
    }

    pub async fn delete_message(
        &self,
        user_token: &str,
        channel: &str,
        ts: &str,
    ) -> Result<ChatDeleteResponse, SlackError> {
        self.json_request(
            "chat.delete",
            user_token,
            &serde_json::json!({
                "channel": channel,
                "ts": ts,
            }),
        )
        .await
    }

    pub async fn get_permalink(
        &self,
        user_token: &str,
        channel: &str,
        message_ts: &str,
    ) -> Result<ChatGetPermalinkResponse, SlackError> {
        self.get_owned_request(
            "chat.getPermalink",
            user_token,
            vec![
                ("channel", channel.to_string()),
                ("message_ts", message_ts.to_string()),
            ],
        )
        .await
    }

    pub async fn create_conversation(
        &self,
        user_token: &str,
        name: &str,
        is_private: bool,
    ) -> Result<ConversationsMutateResponse, SlackError> {
        self.form_owned_request(
            "conversations.create",
            Some(user_token),
            vec![
                ("name", name.to_string()),
                ("is_private", is_private.to_string()),
            ],
        )
        .await
    }

    pub async fn rename_conversation(
        &self,
        user_token: &str,
        channel: &str,
        name: &str,
    ) -> Result<ConversationsMutateResponse, SlackError> {
        self.form_owned_request(
            "conversations.rename",
            Some(user_token),
            vec![("channel", channel.to_string()), ("name", name.to_string())],
        )
        .await
    }

    pub async fn archive_conversation(
        &self,
        user_token: &str,
        channel: &str,
    ) -> Result<(), SlackError> {
        self.form_empty_request(
            "conversations.archive",
            Some(user_token),
            &[("channel", channel)],
        )
        .await
    }

    pub async fn invite_to_conversation(
        &self,
        user_token: &str,
        channel: &str,
        user_id: &str,
    ) -> Result<ConversationsMutateResponse, SlackError> {
        self.form_owned_request(
            "conversations.invite",
            Some(user_token),
            vec![
                ("channel", channel.to_string()),
                ("users", user_id.to_string()),
            ],
        )
        .await
    }

    pub async fn kick_from_conversation(
        &self,
        user_token: &str,
        channel: &str,
        user_id: &str,
    ) -> Result<(), SlackError> {
        self.form_empty_request(
            "conversations.kick",
            Some(user_token),
            &[("channel", channel), ("user", user_id)],
        )
        .await
    }

    pub async fn add_reaction(
        &self,
        user_token: &str,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), SlackError> {
        self.form_empty_request(
            "reactions.add",
            Some(user_token),
            &[
                ("channel", channel),
                ("timestamp", timestamp),
                ("name", name),
            ],
        )
        .await
    }

    pub async fn get_upload_url_external(
        &self,
        user_token: &str,
        filename: &str,
        length: u64,
        alt_text: Option<&str>,
    ) -> Result<GetUploadUrlExternalResponse, SlackError> {
        let mut params = vec![
            ("filename", filename.to_string()),
            ("length", length.to_string()),
        ];
        if let Some(alt_text) = alt_text {
            params.push(("alt_text", alt_text.to_string()));
        }

        self.form_owned_request("files.getUploadURLExternal", Some(user_token), params)
            .await
    }

    pub async fn complete_upload_external(
        &self,
        user_token: &str,
        file_id: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        initial_comment: Option<&str>,
    ) -> Result<CompleteUploadResponse, SlackError> {
        let mut payload = serde_json::json!({
            "files": [{ "id": file_id, "title": file_id }],
            "channel_id": channel_id,
        });

        if let Some(thread_ts) = thread_ts {
            payload["thread_ts"] = serde_json::Value::String(thread_ts.to_string());
        }
        if let Some(initial_comment) = initial_comment {
            payload["initial_comment"] = serde_json::Value::String(initial_comment.to_string());
        }

        self.json_request("files.completeUploadExternal", user_token, &payload)
            .await
    }

    pub async fn upload_external_bytes(
        &self,
        user_token: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        filename: &str,
        bytes: &[u8],
        initial_comment: Option<&str>,
    ) -> Result<CompleteUploadResponse, SlackError> {
        let upload = self
            .get_upload_url_external(user_token, filename, bytes.len() as u64, None)
            .await?;
        self.upload_bytes(&upload.upload_url, bytes).await?;
        self.complete_upload_external(
            user_token,
            &upload.file_id,
            channel_id,
            thread_ts,
            initial_comment,
        )
        .await
    }

    pub async fn upload_bytes(&self, upload_url: &Url, bytes: &[u8]) -> Result<(), SlackError> {
        let response = self
            .http
            .post(upload_url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(SlackError::HttpStatus(response.status().as_u16()))
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, SlackError> {
        self.config.api_base.join(path).map_err(Into::into)
    }

    async fn form_request<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        fields: &[(&str, &str)],
    ) -> Result<T, SlackError> {
        let mut request = self.http.post(self.endpoint(path)?).form(fields);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        decode_ok(response).await
    }

    async fn form_empty_request(
        &self,
        path: &str,
        token: Option<&str>,
        fields: &[(&str, &str)],
    ) -> Result<(), SlackError> {
        let mut request = self.http.post(self.endpoint(path)?).form(fields);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        decode_empty_ok(response).await
    }

    async fn form_owned_request<T: DeserializeOwned>(
        &self,
        path: &str,
        token: Option<&str>,
        fields: Vec<(&str, String)>,
    ) -> Result<T, SlackError> {
        let mut request = self.http.post(self.endpoint(path)?).form(&fields);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        decode_ok(response).await
    }

    async fn json_request<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
        payload: &serde_json::Value,
    ) -> Result<T, SlackError> {
        let response = self
            .http
            .post(self.endpoint(path)?)
            .bearer_auth(token)
            .json(payload)
            .send()
            .await?;
        decode_ok(response).await
    }

    async fn get_owned_request<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
        query: Vec<(&str, String)>,
    ) -> Result<T, SlackError> {
        let mut endpoint = self.endpoint(path)?;
        {
            let mut pairs = endpoint.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, &value);
            }
        }
        let response = self.http.get(endpoint).bearer_auth(token).send().await?;
        decode_ok(response).await
    }
}

async fn decode_ok<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, SlackError> {
    let status = response.status();
    if !status.is_success() {
        return Err(SlackError::HttpStatus(status.as_u16()));
    }

    let raw = response.text().await?;
    let envelope = serde_json::from_str::<SlackEnvelope<T>>(&raw)?;

    if envelope.ok {
        envelope
            .data
            .ok_or_else(|| SlackError::MalformedResponse("missing payload".into()))
    } else {
        Err(SlackError::Api(
            envelope
                .error
                .unwrap_or_else(|| "unknown_slack_error".to_string()),
        ))
    }
}

async fn decode_empty_ok(response: reqwest::Response) -> Result<(), SlackError> {
    let status = response.status();
    if !status.is_success() {
        return Err(SlackError::HttpStatus(status.as_u16()));
    }

    let raw = response.text().await?;
    let envelope = serde_json::from_str::<SlackEnvelope<serde_json::Value>>(&raw)?;

    if envelope.ok {
        Ok(())
    } else {
        Err(SlackError::Api(
            envelope
                .error
                .unwrap_or_else(|| "unknown_slack_error".to_string()),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct SlackEnvelope<T> {
    ok: bool,
    error: Option<String>,
    #[serde(flatten)]
    data: Option<T>,
}

#[derive(Debug, Error)]
pub enum SlackError {
    #[error("slack api error: {0}")]
    Api(String),
    #[error("http request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unexpected http status: {0}")]
    HttpStatus(u16),
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),
    #[error("json decode failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("malformed response: {0}")]
    MalformedResponse(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OAuthAccessResponse {
    pub access_token: Option<String>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub bot_user_id: Option<String>,
    pub app_id: Option<String>,
    pub team: Option<OAuthTeam>,
    pub enterprise: Option<OAuthEnterprise>,
    pub is_enterprise_install: Option<bool>,
    pub authed_user: Option<AuthedUser>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthedUser {
    pub id: Option<String>,
    pub access_token: Option<String>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub token_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OAuthTeam {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OAuthEnterprise {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SocketConnectionResponse {
    pub url: Url,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConversationsListResponse {
    #[serde(default)]
    pub channels: Vec<SlackConversation>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackConversation {
    pub id: String,
    pub name: Option<String>,
    pub name_normalized: Option<String>,
    pub creator: Option<String>,
    pub is_member: Option<bool>,
    pub is_private: Option<bool>,
    pub is_archived: Option<bool>,
}

impl SlackConversation {
    pub fn display_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or_else(|| {
                self.name_normalized
                    .as_deref()
                    .filter(|name| !name.is_empty())
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConversationsHistoryResponse {
    #[serde(default)]
    pub messages: Vec<SlackHistoryMessage>,
    pub has_more: Option<bool>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UsersListResponse {
    #[serde(default)]
    pub members: Vec<SlackUser>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackHistoryMessage {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub subtype: Option<String>,
    pub user: Option<String>,
    pub bot_id: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub blocks: Vec<serde_json::Value>,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub reply_count: Option<u32>,
    pub latest_reply: Option<String>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackUser {
    pub id: String,
    pub name: Option<String>,
    pub deleted: Option<bool>,
    pub is_bot: Option<bool>,
    pub profile: Option<SlackUserProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackUserProfile {
    pub display_name: Option<String>,
    pub display_name_normalized: Option<String>,
    pub real_name: Option<String>,
    pub real_name_normalized: Option<String>,
    pub image_48: Option<String>,
    pub image_72: Option<String>,
    pub image_192: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResponseMetadata {
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatPostMessageResponse {
    pub channel: String,
    pub ts: String,
    pub message: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatUpdateResponse {
    pub channel: String,
    pub ts: String,
    pub text: Option<String>,
    pub message: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatDeleteResponse {
    pub channel: String,
    pub ts: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatGetPermalinkResponse {
    pub permalink: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConversationsMutateResponse {
    pub channel: SlackConversation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackFile {
    pub id: String,
    pub name: Option<String>,
    pub mimetype: Option<String>,
    pub url_private: Option<String>,
    pub url_private_download: Option<String>,
    pub thumb_360: Option<String>,
    pub thumb_720: Option<String>,
    pub permalink: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackReaction {
    pub name: String,
    pub count: u32,
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GetUploadUrlExternalResponse {
    pub upload_url: Url,
    pub file_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompleteUploadResponse {
    pub files: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SocketModeEnvelope {
    #[serde(default)]
    pub envelope_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub connection_info: Option<SocketConnectionInfo>,
    pub payload: Option<EventCallbackEnvelope>,
}

impl SocketModeEnvelope {
    pub fn ack_payload(&self) -> Option<serde_json::Value> {
        self.envelope_id
            .as_ref()
            .map(|envelope_id| serde_json::json!({ "envelope_id": envelope_id }))
    }

    pub fn ack_message(&self) -> Option<Message> {
        self.ack_payload()
            .map(|payload| Message::Text(payload.to_string().into()))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SocketConnectionInfo {
    pub app_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventCallbackEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub event: serde_json::Value,
    pub team_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackMessageEvent {
    pub channel: String,
    pub user: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub blocks: Vec<serde_json::Value>,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub subtype: Option<String>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
}

#[derive(Clone, Debug)]
pub enum SlackSocketEvent {
    Message(SlackMessageEvent),
    MessageChanged(SlackMessageChangedEvent),
    MessageDeleted(SlackMessageDeletedEvent),
    ReactionAdded(SlackReactionEvent),
    ReactionRemoved(SlackReactionEvent),
    UserTyping(SlackUserTypingEvent),
    Unsupported { kind: Option<String> },
}

impl SlackSocketEvent {
    pub fn parse(value: serde_json::Value) -> Result<Self, SlackError> {
        let kind = value
            .get("type")
            .and_then(|kind| kind.as_str())
            .map(ToOwned::to_owned);

        match kind.as_deref() {
            Some("message") => {
                let subtype = value
                    .get("subtype")
                    .and_then(|subtype| subtype.as_str())
                    .map(ToOwned::to_owned);
                match subtype.as_deref() {
                    Some("message_changed") => {
                        Ok(Self::MessageChanged(serde_json::from_value(value)?))
                    }
                    Some("message_deleted") => {
                        Ok(Self::MessageDeleted(serde_json::from_value(value)?))
                    }
                    _ => Ok(Self::Message(serde_json::from_value(value)?)),
                }
            }
            Some("reaction_added") => Ok(Self::ReactionAdded(serde_json::from_value(value)?)),
            Some("reaction_removed") => Ok(Self::ReactionRemoved(serde_json::from_value(value)?)),
            Some("user_typing") => Ok(Self::UserTyping(serde_json::from_value(value)?)),
            Some(_) => Ok(Self::Unsupported { kind }),
            None => Err(SlackError::MalformedResponse(
                "socket mode event payload missing type".to_string(),
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SlackReactionEvent {
    pub user: Option<String>,
    pub reaction: String,
    pub item: SlackReactionItem,
    pub item_user: Option<String>,
    pub event_ts: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SlackReactionItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub channel: Option<String>,
    pub ts: Option<String>,
    pub file: Option<String>,
    pub file_comment: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackMessageChangedEvent {
    pub channel: String,
    pub message: SlackUpdatedMessage,
    pub previous_message: Option<SlackUpdatedMessage>,
    pub event_ts: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackMessageDeletedEvent {
    pub channel: String,
    pub deleted_ts: Option<String>,
    pub previous_message: Option<SlackUpdatedMessage>,
    pub event_ts: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackUpdatedMessage {
    pub user: Option<String>,
    pub bot_id: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub blocks: Vec<serde_json::Value>,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub subtype: Option<String>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SlackUserTypingEvent {
    pub channel: String,
    pub user: Option<String>,
}

pub struct SocketModeSession {
    stream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl SocketModeSession {
    pub async fn connect(client: &SlackClient, app_token: &str) -> Result<Self, SlackError> {
        let connection = client.open_socket_connection(app_token).await?;
        let (stream, _) = connect_async(connection.url.as_str()).await?;
        Ok(Self { stream })
    }

    pub async fn next_envelope(&mut self) -> Result<Option<SocketModeEnvelope>, SlackError> {
        while let Some(message) = self.stream.next().await {
            match message? {
                Message::Text(text) => {
                    let envelope = serde_json::from_str::<SocketModeEnvelope>(text.as_ref())?;
                    return Ok(Some(envelope));
                }
                Message::Ping(payload) => {
                    self.stream.send(Message::Pong(payload)).await?;
                }
                Message::Close(_) => return Ok(None),
                _ => {}
            }
        }

        Ok(None)
    }

    pub async fn ack(&mut self, envelope: &SocketModeEnvelope) -> Result<(), SlackError> {
        let Some(message) = envelope.ack_message() else {
            return Ok(());
        };
        self.stream.send(message).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Message, SlackClient, SlackMessageEvent, SlackOAuthConfig, SlackSocketEvent,
        SocketModeEnvelope,
    };

    #[test]
    fn authorize_url_contains_expected_query() {
        let config = SlackOAuthConfig::new("abc", "def").unwrap();
        let client = SlackClient::new(config);

        let url = client
            .authorize_url(
                &["channels:read", "groups:history", "chat:write"],
                "opaque-state",
            )
            .unwrap();

        let raw = url.as_str();
        assert!(raw.contains("client_id=abc"));
        assert!(raw.contains("user_scope=channels%3Aread%2Cgroups%3Ahistory%2Cchat%3Awrite"));
        assert!(raw.contains("state=opaque-state"));
    }

    #[test]
    fn socket_mode_ack_payload_is_minimal() {
        let envelope = SocketModeEnvelope {
            envelope_id: Some("abc".into()),
            kind: "events_api".into(),
            connection_info: None,
            payload: None,
        };

        assert_eq!(
            envelope.ack_payload(),
            Some(serde_json::json!({ "envelope_id": "abc" }))
        );
        assert!(matches!(envelope.ack_message(), Some(Message::Text(_))));
    }

    #[test]
    fn socket_mode_hello_deserializes_without_ack() {
        let envelope = serde_json::from_str::<SocketModeEnvelope>(
            r#"{
                "type": "hello",
                "connection_info": { "app_id": "A1" },
                "num_connections": 1
            }"#,
        )
        .unwrap();

        assert_eq!(envelope.kind, "hello");
        assert_eq!(envelope.envelope_id, None);
        assert!(envelope.payload.is_none());
        assert_eq!(envelope.ack_payload(), None);
        assert!(!matches!(envelope.ack_message(), Some(Message::Text(_))));
    }

    #[test]
    fn message_events_deserialize() {
        let event = serde_json::from_str::<SlackMessageEvent>(
            r#"{
                "channel": "C1",
                "user": "U1",
                "text": "hello",
                "ts": "123.456",
                "thread_ts": "123.000"
            }"#,
        )
        .unwrap();

        assert_eq!(event.channel, "C1");
        assert_eq!(event.user.as_deref(), Some("U1"));
    }

    #[test]
    fn reaction_added_events_parse() {
        let event = SlackSocketEvent::parse(serde_json::json!({
            "type": "reaction_added",
            "user": "U-reactor",
            "reaction": "thumbsup",
            "item_user": "U-author",
            "item": {
                "type": "message",
                "channel": "C1",
                "ts": "123.456"
            },
            "event_ts": "123.789"
        }))
        .unwrap();

        match event {
            SlackSocketEvent::ReactionAdded(event) => {
                assert_eq!(event.reaction, "thumbsup");
                assert_eq!(event.item.channel.as_deref(), Some("C1"));
                assert_eq!(event.item.ts.as_deref(), Some("123.456"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn message_changed_events_parse() {
        let event = SlackSocketEvent::parse(serde_json::json!({
            "type": "message",
            "subtype": "message_changed",
            "channel": "C1",
            "message": {
                "type": "message",
                "user": "U1",
                "text": "updated",
                "ts": "123.456",
                "reactions": [{ "name": "thumbsup", "count": 1, "users": ["U2"] }]
            }
        }))
        .unwrap();

        assert!(matches!(
            event,
            SlackSocketEvent::MessageChanged(event)
            if event.channel == "C1"
                && event.message.ts == "123.456"
                && event.message.reactions.len() == 1
        ));
    }

    #[test]
    fn typing_events_parse() {
        let event = SlackSocketEvent::parse(serde_json::json!({
            "type": "user_typing",
            "channel": "C1",
            "user": "U1"
        }))
        .unwrap();

        assert!(matches!(
            event,
            SlackSocketEvent::UserTyping(event)
            if event.channel == "C1" && event.user.as_deref() == Some("U1")
        ));
    }
}

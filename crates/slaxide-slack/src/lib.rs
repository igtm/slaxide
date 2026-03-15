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
            redirect_uri: Url::parse("http://127.0.0.1:38080/slack/callback")?,
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
    pub access_token: String,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub authed_user: Option<AuthedUser>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthedUser {
    pub id: Option<String>,
    pub access_token: Option<String>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SocketConnectionResponse {
    pub url: Url,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatPostMessageResponse {
    pub channel: String,
    pub ts: String,
    pub message: serde_json::Value,
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
    pub envelope_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Option<EventCallbackEnvelope>,
}

impl SocketModeEnvelope {
    pub fn ack_payload(&self) -> serde_json::Value {
        serde_json::json!({ "envelope_id": self.envelope_id })
    }

    pub fn ack_message(&self) -> Message {
        Message::Text(self.ack_payload().to_string().into())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventCallbackEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub event: SlackMessageEvent,
    pub team_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SlackMessageEvent {
    pub channel: String,
    pub user: Option<String>,
    pub text: Option<String>,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub subtype: Option<String>,
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
        self.stream.send(envelope.ack_message()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, SlackClient, SlackMessageEvent, SlackOAuthConfig, SocketModeEnvelope};

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
            envelope_id: "abc".into(),
            kind: "events_api".into(),
            payload: None,
        };

        assert_eq!(
            envelope.ack_payload(),
            serde_json::json!({ "envelope_id": "abc" })
        );
        assert!(matches!(envelope.ack_message(), Message::Text(_)));
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
}

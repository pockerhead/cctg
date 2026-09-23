//! Thin Telegram Bot API client: one POST per method, narrow response types.
//!
//! The token lives only in the request URL. Every `reqwest::Error` is stripped
//! of its URL before it leaves this module, and response bodies are decoded by
//! hand, so no error value can carry the token.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Value, json};

use super::config::BotToken;

pub const TELEGRAM_API: &str = "https://api.telegram.org";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// Extra time on top of the long-poll timeout before the HTTP request gives up.
const POLL_GRACE: Duration = Duration::from_secs(15);
/// Used only when Telegram answers 429 without `retry_after`.
const FALLBACK_RETRY_AFTER: Duration = Duration::from_secs(5);
const MIN_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("telegram request failed: {0}")]
    Http(#[source] reqwest::Error),
    #[error("telegram flood control: retry after {0:?}")]
    RetryAfter(Duration),
    #[error("telegram error {code}: {description}")]
    Telegram { code: i64, description: String },
    #[error("unexpected telegram response: {0}")]
    Decode(#[source] serde_json::Error),
}

impl ApiError {
    fn http(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<ResponseParameters>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResponseParameters {
    retry_after: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: i64,
    pub is_bot: bool,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Chat {
    pub id: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Message {
    pub message_id: i64,
    pub message_thread_id: Option<i64>,
    pub is_topic_message: bool,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    /// In a forum topic every message that is not an explicit reply points
    /// at the topic root (the `forum_topic_created` message).
    pub reply_to_message: Option<MessageRef>,
    pub forum_topic_created: Option<IgnoredAny>,
    pub forum_topic_edited: Option<IgnoredAny>,
    pub forum_topic_closed: Option<IgnoredAny>,
    pub forum_topic_reopened: Option<IgnoredAny>,
}

/// Only the id of a message another message refers to.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct MessageRef {
    pub message_id: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CallbackQuery {
    pub id: String,
    pub from: Option<User>,
    pub data: Option<String>,
    /// May be an `InaccessibleMessage` (`date: 0`); only the shared fields are read.
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChatMember {
    pub status: String,
    pub can_manage_topics: bool,
    pub can_delete_messages: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ForumTopic {
    pub message_thread_id: i64,
    pub name: String,
    pub icon_custom_emoji_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Sticker {
    pub custom_emoji_id: Option<String>,
    pub emoji: Option<String>,
}

/// A document for `sendDocument`.
#[derive(Debug, Clone)]
pub struct Document {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub caption: Option<String>,
}

/// Bot API client bound to one bot and one forum supergroup.
pub struct BotApi {
    http: reqwest::Client,
    /// `<api>/bot<token>`; never logged, see the manual `Debug`.
    base: String,
    chat_id: i64,
}

impl fmt::Debug for BotApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotApi { .. }")
    }
}

impl BotApi {
    pub fn new(token: &BotToken, chat_id: i64) -> Result<Self, ApiError> {
        Self::with_api_url(TELEGRAM_API, token, chat_id)
    }

    /// Same as [`BotApi::new`] against another Bot API server (tests, local server).
    pub fn with_api_url(api_url: &str, token: &BotToken, chat_id: i64) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(ApiError::http)?;
        Ok(Self {
            http,
            base: format!("{}/bot{}", api_url.trim_end_matches('/'), token.expose()),
            chat_id,
        })
    }

    pub fn chat_id(&self) -> i64 {
        self.chat_id
    }

    pub async fn get_me(&self) -> Result<User, ApiError> {
        self.call("getMe", json!({}), None).await
    }

    pub async fn get_chat_member(&self, user_id: i64) -> Result<ChatMember, ApiError> {
        let body = json!({ "chat_id": self.chat_id, "user_id": user_id });
        self.call("getChatMember", body, None).await
    }

    /// Long polling. Returns raw updates so that one malformed update cannot
    /// fail the whole batch; see `updates::route_batch`.
    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> Result<Vec<Value>, ApiError> {
        let mut body = json!({
            "timeout": timeout.as_secs(),
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(offset) = offset {
            body["offset"] = json!(offset);
        }
        self.call("getUpdates", body, Some(timeout + POLL_GRACE))
            .await
    }

    pub async fn send_message(
        &self,
        thread_id: Option<i64>,
        text: &str,
        reply_markup: Option<&Value>,
    ) -> Result<Message, ApiError> {
        let mut body = json!({ "chat_id": self.chat_id, "text": text });
        if let Some(thread_id) = thread_id {
            body["message_thread_id"] = json!(thread_id);
        }
        if let Some(markup) = reply_markup {
            body["reply_markup"] = markup.clone();
        }
        self.call("sendMessage", body, None).await
    }

    pub async fn edit_message_text(
        &self,
        message_id: i64,
        text: &str,
        reply_markup: Option<&Value>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "chat_id": self.chat_id, "message_id": message_id, "text": text });
        if let Some(markup) = reply_markup {
            body["reply_markup"] = markup.clone();
        }
        self.call::<IgnoredAny>("editMessageText", body, None)
            .await
            .map(drop)
    }

    pub async fn send_document(
        &self,
        thread_id: Option<i64>,
        document: &Document,
    ) -> Result<Message, ApiError> {
        let part = reqwest::multipart::Part::bytes(document.bytes.clone())
            .file_name(document.file_name.clone());
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.to_string())
            .part("document", part);
        if let Some(thread_id) = thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }
        if let Some(caption) = &document.caption {
            form = form.text("caption", caption.clone());
        }
        let response = self
            .http
            .post(format!("{}/sendDocument", self.base))
            .multipart(form)
            .timeout(DOCUMENT_TIMEOUT)
            .send()
            .await
            .map_err(ApiError::http)?;
        decode(response).await
    }

    pub async fn delete_message(&self, message_id: i64) -> Result<(), ApiError> {
        let body = json!({ "chat_id": self.chat_id, "message_id": message_id });
        self.call::<IgnoredAny>("deleteMessage", body, None)
            .await
            .map(drop)
    }

    pub async fn answer_callback_query(
        &self,
        query_id: &str,
        text: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "callback_query_id": query_id });
        if let Some(text) = text {
            body["text"] = json!(text);
        }
        self.call::<IgnoredAny>("answerCallbackQuery", body, None)
            .await
            .map(drop)
    }

    pub async fn create_forum_topic(
        &self,
        name: &str,
        icon_custom_emoji_id: Option<&str>,
    ) -> Result<ForumTopic, ApiError> {
        let mut body = json!({ "chat_id": self.chat_id, "name": name });
        if let Some(icon) = icon_custom_emoji_id {
            body["icon_custom_emoji_id"] = json!(icon);
        }
        self.call("createForumTopic", body, None).await
    }

    pub async fn edit_forum_topic(
        &self,
        thread_id: i64,
        name: Option<&str>,
        icon_custom_emoji_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "chat_id": self.chat_id, "message_thread_id": thread_id });
        if let Some(name) = name {
            body["name"] = json!(name);
        }
        if let Some(icon) = icon_custom_emoji_id {
            body["icon_custom_emoji_id"] = json!(icon);
        }
        self.call::<IgnoredAny>("editForumTopic", body, None)
            .await
            .map(drop)
    }

    pub async fn get_forum_topic_icon_stickers(&self) -> Result<Vec<Sticker>, ApiError> {
        self.call("getForumTopicIconStickers", json!({}), None)
            .await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        body: Value,
        timeout: Option<Duration>,
    ) -> Result<T, ApiError> {
        let mut request = self
            .http
            .post(format!("{}/{method}", self.base))
            .json(&body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send().await.map_err(ApiError::http)?;
        decode(response).await
    }
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ApiError> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(ApiError::http)?;
    parse_envelope(status.as_u16(), &bytes)
}

/// Maps a Bot API response body to a result. Pure, so it is unit-tested.
fn parse_envelope<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, ApiError> {
    let envelope: Envelope<T> = match serde_json::from_slice(body) {
        Ok(envelope) => envelope,
        Err(error) if (200..300).contains(&status) => return Err(ApiError::Decode(error)),
        Err(_) => {
            return Err(ApiError::Telegram {
                code: i64::from(status),
                description: "non-JSON error response".to_owned(),
            });
        }
    };

    if envelope.ok {
        return envelope.result.ok_or_else(|| ApiError::Telegram {
            code: i64::from(status),
            description: "ok response without result".to_owned(),
        });
    }

    let code = envelope.error_code.unwrap_or(i64::from(status));
    let retry_after = envelope.parameters.and_then(|p| p.retry_after);
    if code == 429 || retry_after.is_some() {
        let wait = retry_after
            .map(Duration::from_secs)
            .unwrap_or(FALLBACK_RETRY_AFTER)
            .max(MIN_RETRY_AFTER);
        return Err(ApiError::RetryAfter(wait));
    }
    Err(ApiError::Telegram {
        code,
        description: envelope
            .description
            .unwrap_or_else(|| "no description".to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key set of a real `getChatMember` answer for the bot (2026-09-23),
    /// values replaced.
    const ADMIN_MEMBER: &str = r#"{"ok":true,"result":{
        "user":{"id":42,"is_bot":true,"first_name":"x","username":"x_bot"},
        "status":"administrator","can_be_edited":false,"can_manage_chat":true,
        "can_change_info":true,"can_delete_messages":true,"can_invite_users":true,
        "can_restrict_members":true,"can_pin_messages":true,"can_manage_topics":true,
        "can_promote_members":true,"can_manage_video_chats":true,"can_post_stories":false,
        "can_edit_stories":false,"can_delete_stories":false,"can_manage_tags":false,
        "can_send_welcome_messages":true,"is_anonymous":true,"can_manage_voice_chats":true}}"#;

    #[test]
    fn decodes_ok_result_and_ignores_unknown_fields() {
        let member: ChatMember = parse_envelope(200, ADMIN_MEMBER.as_bytes()).unwrap();
        assert_eq!(member.status, "administrator");
        assert!(member.can_manage_topics && member.can_delete_messages);
    }

    #[test]
    fn maps_429_to_retry_after() {
        let body = br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#;
        let error = parse_envelope::<IgnoredAny>(429, body).unwrap_err();
        assert!(matches!(error, ApiError::RetryAfter(d) if d == Duration::from_secs(7)));

        let bare = br#"{"ok":false,"error_code":429,"description":"Too Many Requests"}"#;
        let error = parse_envelope::<IgnoredAny>(429, bare).unwrap_err();
        assert!(matches!(error, ApiError::RetryAfter(d) if d == FALLBACK_RETRY_AFTER));

        let zero = br#"{"ok":false,"error_code":429,"parameters":{"retry_after":0}}"#;
        let error = parse_envelope::<IgnoredAny>(429, zero).unwrap_err();
        assert!(matches!(error, ApiError::RetryAfter(d) if d == MIN_RETRY_AFTER));
    }

    #[test]
    fn maps_other_errors_with_description() {
        let body =
            br#"{"ok":false,"error_code":400,"description":"Bad Request: PARTICIPANT_ID_INVALID"}"#;
        let error = parse_envelope::<IgnoredAny>(400, body).unwrap_err();
        assert!(
            matches!(error, ApiError::Telegram { code: 400, ref description } if description.contains("PARTICIPANT_ID_INVALID"))
        );

        let html = parse_envelope::<IgnoredAny>(502, b"<html>bad gateway</html>").unwrap_err();
        assert!(matches!(html, ApiError::Telegram { code: 502, .. }));

        let garbage = parse_envelope::<User>(200, b"{").unwrap_err();
        assert!(matches!(garbage, ApiError::Decode(_)));
    }

    #[tokio::test]
    async fn transport_errors_never_contain_the_token() {
        // Ephemeral token built at run time; port 9 (discard) on loopback is closed.
        let secret = format!("leak-marker-{}", std::process::id());
        let token = super::super::config::Config::from_vars(|name| match name {
            "CCTG_BOT_TOKEN" => Some(format!("777:{secret}")),
            "CCTG_CHAT_ID" => Some("-1001".to_owned()),
            "CCTG_ALLOWED_USER_IDS" => Some("1".to_owned()),
            _ => None,
        })
        .unwrap()
        .token;
        let api = BotApi::with_api_url("http://127.0.0.1:9", &token, -1001).unwrap();

        let error = api.get_me().await.unwrap_err();
        assert!(matches!(error, ApiError::Http(_)));
        let rendered = [
            format!("{error}"),
            format!("{error:?}"),
            format!("{:#}", anyhow::Error::from(error)),
            format!("{api:?}"),
        ];
        for text in rendered {
            assert!(!text.contains(&secret), "token leaked: {text}");
            assert!(!text.contains("777:"), "bot id leaked: {text}");
        }
    }
}

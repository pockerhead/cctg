//! Thin Telegram Bot API client: one POST per method, narrow response types.
//!
//! The token lives only in the request URL, and in the URL of a file download
//! (`<api>/file/bot<token>/<file_path>`). Every `reqwest::Error` is stripped
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
    pub first_name: Option<String>,
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
    /// Boxed: keeps `Message` (a scheduler `Outcome`) small.
    pub from: Option<Box<User>>,
    pub chat: Chat,
    pub text: Option<String>,
    /// In a forum topic every message that is not an explicit reply points
    /// at the topic root (the `forum_topic_created` message).
    pub reply_to_message: Option<MessageRef>,
    /// The part of the replied message the user selected (`TextQuote`).
    pub quote: Option<TextQuote>,
    /// Present on a forwarded message (`MessageOrigin`); only its presence
    /// is read.
    pub forward_origin: Option<IgnoredAny>,
    pub forum_topic_created: Option<IgnoredAny>,
    pub forum_topic_edited: Option<IgnoredAny>,
    pub forum_topic_closed: Option<IgnoredAny>,
    pub forum_topic_reopened: Option<IgnoredAny>,
    /// A `pinned_message` service message: the message that was pinned.
    pub pinned_message: Option<MessageRef>,
    /// The file of the message and its caption (TASK-032). Boxed: a sent
    /// message in every scheduler answer stays small.
    #[serde(flatten)]
    pub media: Box<MessageMedia>,
}

/// The file fields of a message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageMedia {
    /// The words that came with a file.
    pub caption: Option<String>,
    /// Every size of a photo, the largest usually last.
    pub photo: Option<Vec<PhotoSize>>,
    /// A GIF or a silent video; Telegram also fills `document` for it.
    pub animation: Option<FileInfo>,
    pub document: Option<FileInfo>,
    pub video: Option<FileInfo>,
    pub voice: Option<FileInfo>,
    pub audio: Option<FileInfo>,
}

/// The fields of `Document`, `Video`, `Voice`, `Audio` and `Animation` the
/// hub reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileInfo {
    pub file_id: String,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: u64,
    pub height: u64,
    pub file_size: Option<u64>,
}

/// A `getFile` answer: where to download the file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct File {
    pub file_size: Option<u64>,
    pub file_path: Option<String>,
}

/// What Telegram answers when a bot asks for a file bigger than it may
/// download; a download past the limit fails the same way.
pub const FILE_TOO_BIG: &str = "Bad Request: file is too big";

impl ApiError {
    /// The file is bigger than a bot may download.
    pub fn is_too_big(&self) -> bool {
        matches!(self, Self::Telegram { code: 400, description } if description.to_ascii_lowercase().contains("file is too big"))
    }

    /// A 400 about the picture itself (`PHOTO_INVALID_DIMENSIONS`,
    /// `IMAGE_PROCESS_FAILED`, ...): the same file may still go as a
    /// document. A missing topic or chat is not one.
    pub fn is_photo_refusal(&self) -> bool {
        matches!(self, Self::Telegram { code: 400, description } if {
            let description = description.to_ascii_uppercase();
            description.contains("PHOTO") || description.contains("IMAGE")
        })
    }
}

/// The id and the words of a message another message refers to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageRef {
    pub message_id: i64,
    pub text: Option<String>,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TextQuote {
    pub text: String,
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
    pub can_pin_messages: bool,
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
    /// `<api>/file/bot<token>`, for downloads; never logged either.
    file_base: String,
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
        let api_url = api_url.trim_end_matches('/');
        Ok(Self {
            http,
            base: format!("{api_url}/bot{}", token.expose()),
            file_base: format!("{api_url}/file/bot{}", token.expose()),
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

    /// `parse_mode`: `Some("HTML")` sends `text` as Telegram HTML; `None` as plain text.
    /// `reply_to`: the message this one answers; Telegram refuses the send
    /// when that message is gone.
    /// `notify: false` sends it with `disable_notification` (no sound).
    pub async fn send_message(
        &self,
        thread_id: Option<i64>,
        text: &str,
        reply_markup: Option<&Value>,
        parse_mode: Option<&str>,
        reply_to: Option<i64>,
        notify: bool,
    ) -> Result<Message, ApiError> {
        let mut body = json!({ "chat_id": self.chat_id, "text": text });
        if !notify {
            body["disable_notification"] = json!(true);
        }
        if let Some(thread_id) = thread_id {
            body["message_thread_id"] = json!(thread_id);
        }
        if let Some(reply_to) = reply_to {
            body["reply_parameters"] = json!({ "message_id": reply_to });
        }
        if let Some(parse_mode) = parse_mode {
            body["parse_mode"] = json!(parse_mode);
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

    /// `notify`: as in [`Self::send_message`].
    pub async fn send_document(
        &self,
        thread_id: Option<i64>,
        document: &Document,
        notify: bool,
    ) -> Result<Message, ApiError> {
        self.send_file("sendDocument", "document", thread_id, document, notify)
            .await
    }

    /// `sendPhoto` of a JPEG, PNG or WebP of at most 10 MB (TASK-032).
    pub async fn send_photo(
        &self,
        thread_id: Option<i64>,
        document: &Document,
        notify: bool,
    ) -> Result<Message, ApiError> {
        self.send_file("sendPhoto", "photo", thread_id, document, notify)
            .await
    }

    /// `sendMediaGroup` (TASK-059): 2-10 files as one album, all photos
    /// (`photos`) or all documents, which Telegram never mixes; each file is
    /// the multipart field `file<i>`, named by `attach://file<i>`. Answers
    /// the first message of the album.
    pub async fn send_media_group(
        &self,
        thread_id: Option<i64>,
        items: &[Document],
        photos: bool,
        notify: bool,
    ) -> Result<Message, ApiError> {
        let kind = if photos { "photo" } else { "document" };
        let mut media = Vec::with_capacity(items.len());
        let mut form = reqwest::multipart::Form::new().text("chat_id", self.chat_id.to_string());
        for (index, item) in items.iter().enumerate() {
            let field = format!("file{index}");
            let mut entry = json!({ "type": kind, "media": format!("attach://{field}") });
            if let Some(caption) = &item.caption {
                entry["caption"] = json!(caption);
            }
            media.push(entry);
            let part = reqwest::multipart::Part::bytes(item.bytes.clone())
                .file_name(item.file_name.clone());
            form = form.part(field, part);
        }
        form = form.text("media", Value::Array(media).to_string());
        if !notify {
            form = form.text("disable_notification", "true");
        }
        if let Some(thread_id) = thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }
        let response = self
            .http
            .post(format!("{}/sendMediaGroup", self.base))
            .multipart(form)
            .timeout(DOCUMENT_TIMEOUT)
            .send()
            .await
            .map_err(ApiError::http)?;
        let messages: Vec<Message> = decode(response).await?;
        Ok(messages.into_iter().next().unwrap_or_default())
    }

    /// One multipart upload: the bytes as field `field` of `method`.
    async fn send_file(
        &self,
        method: &str,
        field: &str,
        thread_id: Option<i64>,
        document: &Document,
        notify: bool,
    ) -> Result<Message, ApiError> {
        let part = reqwest::multipart::Part::bytes(document.bytes.clone())
            .file_name(document.file_name.clone());
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.to_string())
            .part(field.to_owned(), part);
        if !notify {
            form = form.text("disable_notification", "true");
        }
        if let Some(thread_id) = thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }
        if let Some(caption) = &document.caption {
            form = form.text("caption", caption.clone());
        }
        let response = self
            .http
            .post(format!("{}/{method}", self.base))
            .multipart(form)
            .timeout(DOCUMENT_TIMEOUT)
            .send()
            .await
            .map_err(ApiError::http)?;
        decode(response).await
    }

    /// `getFile`: where a file of a message can be downloaded.
    pub async fn get_file(&self, file_id: &str) -> Result<File, ApiError> {
        self.call("getFile", json!({ "file_id": file_id }), None)
            .await
    }

    /// Downloads a file by its `getFile` path; more than `limit` bytes fail
    /// as [`FILE_TOO_BIG`]. Errors never carry the URL.
    pub async fn download(&self, file_path: &str, limit: u64) -> Result<Vec<u8>, ApiError> {
        let too_big = || ApiError::Telegram {
            code: 400,
            description: FILE_TOO_BIG.to_owned(),
        };
        let mut response = self
            .http
            .get(format!(
                "{}/{}",
                self.file_base,
                file_path.trim_start_matches('/')
            ))
            .timeout(DOCUMENT_TIMEOUT)
            .send()
            .await
            .map_err(ApiError::http)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::Telegram {
                code: i64::from(status.as_u16()),
                description: "file download failed".to_owned(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(too_big());
        }
        let mut bytes = Vec::new();
        while let Some(piece) = response.chunk().await.map_err(ApiError::http)? {
            if bytes.len() as u64 + piece.len() as u64 > limit {
                return Err(too_big());
            }
            bytes.extend_from_slice(&piece);
        }
        Ok(bytes)
    }

    /// Replaces the bot's reaction on a message with one emoji from the Bot
    /// API list (`👀`, `✍` are in it; bots set at most one reaction).
    pub async fn set_message_reaction(&self, message_id: i64, emoji: &str) -> Result<(), ApiError> {
        let body = json!({
            "chat_id": self.chat_id,
            "message_id": message_id,
            "reaction": [{ "type": "emoji", "emoji": emoji }],
        });
        self.call::<IgnoredAny>("setMessageReaction", body, None)
            .await
            .map(drop)
    }

    /// Pins a message without a notification; a message of a forum topic is
    /// pinned in that topic.
    pub async fn pin_chat_message(&self, message_id: i64) -> Result<(), ApiError> {
        let body = json!({
            "chat_id": self.chat_id,
            "message_id": message_id,
            "disable_notification": true,
        });
        self.call::<IgnoredAny>("pinChatMessage", body, None)
            .await
            .map(drop)
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
        assert!(member.can_manage_topics && member.can_delete_messages && member.can_pin_messages);
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
    fn media_messages_decode_their_files_and_a_too_big_file_is_known() {
        let body = r#"{"ok":true,"result":{"message_id":5,"chat":{"id":-1001},"caption":"c",
            "photo":[{"file_id":"s","file_unique_id":"u1","width":90,"height":60,"file_size":900},
                     {"file_id":"l","file_unique_id":"u2","width":1280,"height":853}],
            "animation":{"file_id":"a","file_unique_id":"u3","file_name":"x.mp4","mime_type":"video/mp4"},
            "document":{"file_id":"a","file_unique_id":"u3","file_size":123,"thumbnail":{}}}}"#;
        let message: Message = parse_envelope(200, body.as_bytes()).unwrap();
        assert_eq!(message.chat.id, -1001);
        let media = message.media;
        assert_eq!(media.caption.as_deref(), Some("c"));
        let photo = media.photo.unwrap();
        assert_eq!(
            (
                photo[1].file_id.as_str(),
                photo[1].width,
                photo[1].file_size
            ),
            ("l", 1280, None)
        );
        assert_eq!(media.animation.unwrap().file_name.as_deref(), Some("x.mp4"));
        assert_eq!(media.document.unwrap().file_size, Some(123));
        let file: File = parse_envelope(
            200,
            br#"{"ok":true,"result":{"file_id":"f","file_unique_id":"u","file_size":7,"file_path":"photos/file_1.jpg"}}"#,
        )
        .unwrap();
        assert_eq!(file.file_path.as_deref(), Some("photos/file_1.jpg"));
        let refused = parse_envelope::<File>(
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad Request: file is too big"}"#,
        )
        .unwrap_err();
        assert!(refused.is_too_big());
        assert!(!ApiError::RetryAfter(Duration::from_secs(1)).is_too_big());
    }

    #[test]
    fn only_a_refusal_of_the_picture_itself_is_a_photo_refusal() {
        let refusal = |code, description: &str| ApiError::Telegram {
            code,
            description: description.to_owned(),
        };
        for description in [
            "Bad Request: PHOTO_INVALID_DIMENSIONS",
            "Bad Request: IMAGE_PROCESS_FAILED",
            "Bad Request: PHOTO_SAVE_FILE_INVALID",
        ] {
            assert!(
                refusal(400, description).is_photo_refusal(),
                "{description}"
            );
        }
        for description in [
            "Bad Request: message thread not found",
            "Bad Request: chat not found",
            "Bad Request: can't parse entities",
        ] {
            assert!(
                !refusal(400, description).is_photo_refusal(),
                "{description}"
            );
        }
        assert!(!refusal(500, "PHOTO_INVALID_DIMENSIONS").is_photo_refusal());
        assert!(!ApiError::RetryAfter(Duration::from_secs(1)).is_photo_refusal());
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

    /// A fake Bot API server that answers its `n`-th connection with
    /// `answers[n]` (a whole raw HTTP answer) after reading the request,
    /// then closes it; returns its url and the request lines it saw.
    async fn fake_server(
        answers: Vec<Vec<u8>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests = seen.clone();
        tokio::spawn(async move {
            for answer in answers {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                requests.lock().unwrap().push(request.trim_end().to_owned());
                let (mut length, mut chunked) = (0, false);
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    let line = line.trim_end().to_ascii_lowercase();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    chunked |= line.starts_with("transfer-encoding:") && line.contains("chunked");
                }
                // The whole body is read before the answer (an early answer
                // can be lost to a reset on Windows).
                if chunked {
                    let mut body = Vec::new();
                    while !body.ends_with(b"0\r\n\r\n") {
                        let mut byte = [0u8];
                        stream.read_exact(&mut byte).await.unwrap();
                        body.push(byte[0]);
                    }
                } else {
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).await.unwrap();
                }
                let mut stream = stream.into_inner();
                stream.write_all(&answer).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        (url, seen)
    }

    fn test_api(url: &str) -> BotApi {
        let token = super::super::config::Config::from_vars(|name| match name {
            "CCTG_BOT_TOKEN" => Some("777:test-token".to_owned()),
            "CCTG_CHAT_ID" => Some("-1001".to_owned()),
            "CCTG_ALLOWED_USER_IDS" => Some("1".to_owned()),
            _ => None,
        })
        .unwrap()
        .token;
        BotApi::with_api_url(url, &token, -1001).unwrap()
    }

    /// A raw HTTP answer with `head` lines, closing the connection.
    fn http_answer(status: &str, head: &str, body: &[u8]) -> Vec<u8> {
        let mut answer =
            format!("HTTP/1.1 {status}\r\n{head}connection: close\r\n\r\n").into_bytes();
        answer.extend_from_slice(body);
        answer
    }

    #[tokio::test]
    async fn a_download_stops_at_its_limit_by_length_or_by_bytes_and_a_refusal_is_told() {
        let (url, seen) = fake_server(vec![
            http_answer("200 OK", "content-length: 5\r\n", b"hello"),
            // Announced bigger than the limit: refused before any byte (the
            // body is cut short, so reading it would fail instead).
            http_answer("200 OK", "content-length: 1000\r\n", b"hello"),
            // No length: the bytes past the limit end it.
            http_answer("200 OK", "", b"hello world"),
            http_answer("404 Not Found", "content-length: 2\r\n", b"{}"),
        ])
        .await;
        let api = test_api(&url);
        assert_eq!(api.download("/photos/f.jpg", 5).await.unwrap(), b"hello");
        assert!(
            api.download("photos/f.jpg", 10)
                .await
                .unwrap_err()
                .is_too_big()
        );
        assert!(
            api.download("photos/f.jpg", 10)
                .await
                .unwrap_err()
                .is_too_big()
        );
        let refused = api.download("photos/f.jpg", 10).await.unwrap_err();
        assert!(
            matches!(&refused, ApiError::Telegram { code: 404, .. }) && !refused.is_too_big(),
            "{refused:?}"
        );
        assert!(!format!("{refused} {refused:?}").contains("test-token"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0], "GET /file/bot777:test-token/photos/f.jpg HTTP/1.1");
    }

    #[tokio::test]
    async fn only_a_refused_picture_goes_again_as_a_document() {
        use crate::hub::scheduler::{Op, Transport};
        let json = |status, body: &str| {
            http_answer(
                status,
                &format!("content-length: {}\r\n", body.len()),
                body.as_bytes(),
            )
        };
        let refused = |description: &str| {
            json(
                "400 Bad Request",
                &format!(r#"{{"ok":false,"error_code":400,"description":"{description}"}}"#),
            )
        };
        let (url, seen) = fake_server(vec![
            refused("Bad Request: PHOTO_INVALID_DIMENSIONS"),
            json(
                "200 OK",
                r#"{"ok":true,"result":{"message_id":5,"chat":{"id":-1001}}}"#,
            ),
            refused("Bad Request: message thread not found"),
            // Taken only by a fallback that should not happen.
            json(
                "200 OK",
                r#"{"ok":true,"result":{"message_id":6,"chat":{"id":-1001}}}"#,
            ),
        ])
        .await;
        let api = test_api(&url);
        let op = Op::SendPhoto {
            thread_id: Some(100),
            document: Document {
                file_name: "shot.png".into(),
                bytes: b"\x89PNG\r\n\x1a\nbytes".to_vec(),
                caption: None,
            },
            notify: false,
        };
        assert!(api.execute(&op).await.is_ok());
        let gone = api.execute(&op).await.unwrap_err();
        assert!(
            matches!(&gone, ApiError::Telegram { code: 400, description } if description.contains("thread not found")),
            "{gone:?}"
        );
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|line| line.split(['/', ' ']).nth(3).unwrap_or_default().to_owned())
            .collect();
        assert_eq!(methods, ["sendPhoto", "sendDocument", "sendPhoto"]);
    }

    /// TASK-059: Telegram refuses a photo album as a whole; on any 400,
    /// whatever its text, the same files then go as a document album in the
    /// same job. A document album and a refusal other than 400 are not sent
    /// again.
    #[tokio::test]
    async fn a_refused_photo_album_goes_again_as_documents() {
        use crate::hub::scheduler::{Op, Outcome, Transport};
        let json = |status, body: &str| {
            http_answer(
                status,
                &format!("content-length: {}\r\n", body.len()),
                body.as_bytes(),
            )
        };
        let album = r#"{"ok":true,"result":[{"message_id":7,"chat":{"id":-1001}},{"message_id":8,"chat":{"id":-1001}}]}"#;
        let refused = |description: &str| {
            json(
                "400 Bad Request",
                &format!(r#"{{"ok":false,"error_code":400,"description":"{description}"}}"#),
            )
        };
        let (url, seen) = fake_server(vec![
            refused("Bad Request: IMAGE_PROCESS_FAILED"),
            json("200 OK", album),
            // A group refusal that names no picture.
            refused("Bad Request: failed to send the media group"),
            json("200 OK", album),
            json(
                "500 Internal Server Error",
                r#"{"ok":false,"error_code":500,"description":"Internal Server Error"}"#,
            ),
            refused("Bad Request: IMAGE_PROCESS_FAILED"),
            // Taken only by a fallback that should not happen.
            json("200 OK", album),
        ])
        .await;
        let api = test_api(&url);
        let items = vec![
            Document {
                file_name: "a.png".into(),
                bytes: b"\x89PNG\r\n\x1a\na".to_vec(),
                caption: Some("two".into()),
            },
            Document {
                file_name: "b.png".into(),
                bytes: b"\x89PNG\r\n\x1a\nb".to_vec(),
                caption: None,
            },
        ];
        let photos = Op::SendAlbum {
            thread_id: Some(100),
            items: items.clone(),
            photos: true,
            notify: false,
        };
        match api.execute(&photos).await {
            Ok(Outcome::Sent(message)) => assert_eq!(message.message_id, 7),
            other => panic!("{other:?}"),
        }
        match api.execute(&photos).await {
            Ok(Outcome::Sent(message)) => assert_eq!(message.message_id, 7),
            other => panic!("any 400 falls back: {other:?}"),
        }
        assert!(api.execute(&photos).await.is_err(), "not a 400");
        let documents = Op::SendAlbum {
            thread_id: Some(100),
            items,
            photos: false,
            notify: false,
        };
        assert!(api.execute(&documents).await.is_err());
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|line| line.split(['/', ' ']).nth(3).unwrap_or_default().to_owned())
            .collect();
        assert_eq!(methods, ["sendMediaGroup"; 6]);
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
        let download = api.download("photos/file_1.jpg", 10).await.unwrap_err();
        assert!(matches!(download, ApiError::Http(_)));
        let rendered = [
            format!("{error}"),
            format!("{error:?}"),
            format!("{:#}", anyhow::Error::from(error)),
            format!("{download} {download:?}"),
            format!("{api:?}"),
        ];
        for text in rendered {
            assert!(!text.contains(&secret), "token leaked: {text}");
            assert!(!text.contains("777:"), "bot id leaked: {text}");
        }
    }
}

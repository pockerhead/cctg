//! Inbound side: long polling, allowlist gate, service-message classification.
//!
//! `Routed` values carry no Telegram user id, so nothing downstream can log one.

use std::future::Future;
use std::io;
use std::pin::pin;
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use super::api::{ApiError, BotApi, FileInfo, Message, Update};
use super::buffer::Attachment;
use super::config::Allowlist;
use super::offset::OffsetStore;
use super::registry::cut;
use crate::wire::FileKind;

const POLL_TIMEOUT: Duration = Duration::from_secs(50);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STALLED_BATCH_BACKOFF: Duration = Duration::from_secs(1);
/// UTF-16 units kept of a replied message quoted for the session.
pub const QUOTE_LIMIT: usize = 500;
/// Waits before the second and third attempt to save the offset (a file held
/// open by a scanner or indexer on Windows makes the rename fail for a moment).
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// A message from an allowlisted user in the configured chat.
    Input(Inbound),
    /// A button press from an allowlisted user.
    Callback(CallbackInput),
    /// A forum service message. Never user input; the hub may delete it.
    Service(ServiceMessage),
    Ignored(Ignored),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub message_id: i64,
    /// `None` for the General topic.
    pub thread_id: Option<i64>,
    pub text: Option<String>,
    /// The message this one explicitly answers. `None` for the implicit
    /// reply to the topic root that Telegram sets on every topic message.
    pub reply_to: Option<i64>,
    /// The words an explicit reply answers: the fragment the user selected,
    /// else the start of the replied text or caption, at most
    /// [`QUOTE_LIMIT`]. Never logged.
    pub quote: Option<String>,
    /// A forwarded message: someone else's words, not the user's.
    pub forwarded: bool,
    /// A file the message carried and its caption (TASK-032); `text` is
    /// then `None`. Never logged but for its kind and size.
    pub media: Option<Media>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub file: Attachment,
    pub caption: Option<String>,
}

/// The file of a message: an animation before its `document` twin, the
/// largest size of a photo. Stickers, video notes and other kinds are none.
fn media(message: &mut Message) -> Option<Media> {
    let file = |kind: FileKind, info: FileInfo| Attachment {
        kind,
        file_id: info.file_id,
        name: info.file_name,
        size: info.file_size,
    };
    let photo = message.media.photo.take().and_then(|sizes| {
        sizes
            .into_iter()
            .max_by_key(|size| (size.width.saturating_mul(size.height), size.file_size))
            .map(|largest| Attachment {
                kind: FileKind::Photo,
                file_id: largest.file_id,
                name: None,
                size: largest.file_size,
            })
    });
    let file = message
        .media
        .animation
        .take()
        .map(|info| file(FileKind::Animation, info))
        .or(photo)
        .or_else(|| {
            message
                .media
                .video
                .take()
                .map(|info| file(FileKind::Video, info))
        })
        .or_else(|| {
            message
                .media
                .voice
                .take()
                .map(|info| file(FileKind::Voice, info))
        })
        .or_else(|| {
            message
                .media
                .audio
                .take()
                .map(|info| file(FileKind::Audio, info))
        })
        .or_else(|| {
            message
                .media
                .document
                .take()
                .map(|info| file(FileKind::Document, info))
        })
        .filter(|file| !file.file_id.is_empty())?;
    Some(Media {
        file,
        caption: message.media.caption.take(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackInput {
    pub query_id: String,
    pub data: Option<String>,
    pub message_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceMessage {
    pub kind: ServiceKind,
    pub message_id: i64,
    pub thread_id: Option<i64>,
    /// Who did it (the bot for its own topic edits and pins). Never logged.
    pub from: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    TopicCreated,
    TopicEdited,
    TopicClosed,
    TopicReopened,
    /// A message was pinned: its id (TASK-029).
    Pinned(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// Sender is not in the allowlist.
    NotAllowed,
    /// No `from` (channel posts and similar).
    NoSender,
    /// Another chat than the configured supergroup.
    OtherChat,
    /// An update type the hub does not handle.
    Unsupported,
    /// The update did not match the expected shape.
    Malformed,
}

fn service_kind(message: &Message) -> Option<ServiceKind> {
    if message.forum_topic_created.is_some() {
        Some(ServiceKind::TopicCreated)
    } else if message.forum_topic_edited.is_some() {
        Some(ServiceKind::TopicEdited)
    } else if message.forum_topic_closed.is_some() {
        Some(ServiceKind::TopicClosed)
    } else if message.forum_topic_reopened.is_some() {
        Some(ServiceKind::TopicReopened)
    } else {
        message
            .pinned_message
            .as_ref()
            .map(|pinned| ServiceKind::Pinned(pinned.message_id))
    }
}

/// Classifies one parsed update. Service messages are recognised before the
/// allowlist check because the bot itself is their sender.
pub fn classify(update: Update, chat_id: i64, allowlist: &Allowlist) -> Routed {
    if let Some(mut message) = update.message {
        if message.chat.id != chat_id {
            return Routed::Ignored(Ignored::OtherChat);
        }
        if let Some(kind) = service_kind(&message) {
            return Routed::Service(ServiceMessage {
                kind,
                message_id: message.message_id,
                thread_id: message.message_thread_id,
                from: message.from.as_ref().map(|from| from.id),
            });
        }
        let Some(from) = message.from.take() else {
            return Routed::Ignored(Ignored::NoSender);
        };
        if !allowlist.contains(from.id) {
            return Routed::Ignored(Ignored::NotAllowed);
        }
        let media = media(&mut message);
        let thread_id = message
            .message_thread_id
            .filter(|_| message.is_topic_message);
        let replied = message
            .reply_to_message
            .filter(|replied| replied.message_id != 0 && Some(replied.message_id) != thread_id);
        let reply_to = replied.as_ref().map(|replied| replied.message_id);
        let quote = replied.and_then(|replied| {
            let words = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
            words(message.quote.map(|quote| quote.text))
                .or_else(|| words(replied.text))
                .or_else(|| words(replied.caption))
                .map(|text| cut(&text, QUOTE_LIMIT))
        });
        return Routed::Input(Inbound {
            message_id: message.message_id,
            thread_id,
            text: message.text,
            reply_to,
            quote,
            forwarded: message.forward_origin.is_some(),
            media,
        });
    }

    if let Some(query) = update.callback_query {
        if query
            .message
            .as_ref()
            .is_some_and(|message| message.chat.id != chat_id)
        {
            return Routed::Ignored(Ignored::OtherChat);
        }
        let Some(from) = query.from else {
            return Routed::Ignored(Ignored::NoSender);
        };
        if !allowlist.contains(from.id) {
            return Routed::Ignored(Ignored::NotAllowed);
        }
        return Routed::Callback(CallbackInput {
            query_id: query.id,
            data: query.data,
            message_id: query.message.map(|message| message.message_id),
        });
    }

    Routed::Ignored(Ignored::Unsupported)
}

/// Routes a raw `getUpdates` batch. Returns the next offset and the routed
/// updates. A malformed update is skipped, not fatal, and still advances the
/// offset so it is not fetched again.
///
/// The next offset is one past the highest `update_id` of this batch, even
/// when that is below `offset`: after a week without updates Telegram picks
/// the next id at random, and keeping the old, higher offset would fetch and
/// handle the same update again on every call.
pub fn route_batch(
    raw: Vec<Value>,
    offset: Option<i64>,
    chat_id: i64,
    allowlist: &Allowlist,
) -> (Option<i64>, Vec<Routed>) {
    let mut highest: Option<i64> = None;
    let mut routed = Vec::with_capacity(raw.len());
    for value in raw {
        if let Some(id) = value.get("update_id").and_then(Value::as_i64) {
            highest = Some(highest.map_or(id, |current| current.max(id)));
        }
        let item = match serde_json::from_value::<Update>(value) {
            Ok(update) => classify(update, chat_id, allowlist),
            Err(_) => Routed::Ignored(Ignored::Malformed),
        };
        match &item {
            Routed::Ignored(reason) => debug!(?reason, "update ignored"),
            Routed::Service(service) => debug!(kind = ?service.kind, "forum service message"),
            Routed::Input(_) | Routed::Callback(_) => {}
        }
        routed.push(item);
    }
    let next = highest.map(|id| id.saturating_add(1)).or(offset);
    (next, routed)
}

/// Where updates come from: `BotApi` in production, a fake in tests.
pub trait UpdateSource {
    fn chat_id(&self) -> i64;
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<Value>, ApiError>> + Send;
}

impl UpdateSource for BotApi {
    fn chat_id(&self) -> i64 {
        BotApi::chat_id(self)
    }

    async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> Result<Vec<Value>, ApiError> {
        BotApi::get_updates(self, offset, timeout).await
    }
}

fn stalled_batch_backoff(
    batch_len: usize,
    previous: Option<i64>,
    next: Option<i64>,
) -> Option<Duration> {
    (batch_len > 0 && next == previous).then_some(STALLED_BATCH_BACKOFF)
}

/// Saves `offset`, retrying twice. A save that still fails is logged and the
/// poll goes on: an unwritable state directory must not stop the hub; the
/// cost is that a restart before the next successful save repeats the batch.
async fn save_offset_once(store: &OffsetStore, offset: i64) -> io::Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save(offset))
        .await
        .map_err(|_| io::Error::other("offset save worker failed"))?
}

async fn save_offset(store: &OffsetStore, offset: i64) {
    let mut result = save_offset_once(store, offset).await;
    for wait in SAVE_RETRY_WAITS {
        if result.is_ok() {
            return;
        }
        tokio::time::sleep(wait).await;
        result = save_offset_once(store, offset).await;
    }
    if let Err(error) = result {
        warn!(kind = ?error.kind(), "cannot save the getUpdates offset; a restart may repeat this batch");
    }
}

/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
///
/// Starts from the offset in `store` and saves each new offset before the
/// batch is handled: a crash in between skips those updates instead of
/// handling them twice (at most once, so a command is never answered twice).
pub async fn poll<S: UpdateSource>(
    source: &S,
    allowlist: &Allowlist,
    store: &OffsetStore,
    handle: impl FnMut(Routed),
) {
    poll_until(source, allowlist, store, handle, std::future::pending()).await;
}

/// [`poll`] until `stop` completes. It is only noticed while no batch is
/// being handled: a batch whose offset was saved is always handed out whole,
/// and an interrupted `getUpdates` confirms nothing, so its updates come again.
pub async fn poll_until<S: UpdateSource>(
    source: &S,
    allowlist: &Allowlist,
    store: &OffsetStore,
    mut handle: impl FnMut(Routed),
    stop: impl Future<Output = ()>,
) {
    let mut stop = pin!(stop);
    let mut offset = store.load();
    let mut backoff = Duration::from_secs(1);
    loop {
        let result = tokio::select! {
            biased;
            () = &mut stop => return,
            result = source.get_updates(offset, POLL_TIMEOUT) => result,
        };
        let wait = match result {
            Ok(raw) => {
                backoff = Duration::from_secs(1);
                let batch_len = raw.len();
                let previous = offset;
                let (next, routed) = route_batch(raw, offset, source.chat_id(), allowlist);
                offset = next;
                if next != previous
                    && let Some(next) = next
                {
                    save_offset(store, next).await;
                }
                routed.into_iter().for_each(&mut handle);
                let wait = stalled_batch_backoff(batch_len, previous, next);
                if let Some(wait) = wait {
                    warn!(?wait, "getUpdates batch did not contain an update_id");
                }
                wait
            }
            Err(ApiError::RetryAfter(wait)) => {
                warn!(?wait, "getUpdates hit flood control");
                Some(wait)
            }
            Err(error) => {
                warn!(%error, ?backoff, "getUpdates failed");
                let wait = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                Some(wait)
            }
        };
        if let Some(wait) = wait {
            tokio::select! {
                biased;
                () = &mut stop => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CHAT: i64 = -1000000000001;
    const ALLOWED: i64 = 1001;
    const STRANGER: i64 = 2002;
    const BOT: i64 = 3003;

    fn allowlist() -> Allowlist {
        [ALLOWED].into_iter().collect()
    }

    fn message(from: i64, extra: Value) -> Value {
        let mut message = json!({
            "message_id": 10,
            "message_thread_id": 7,
            "is_topic_message": true,
            "date": 1,
            "from": { "id": from, "is_bot": false, "first_name": "x" },
            "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
        });
        if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        message
    }

    fn route_one(update: Value) -> Routed {
        let (_, mut routed) = route_batch(vec![update], None, CHAT, &allowlist());
        routed.remove(0)
    }

    #[test]
    fn allowlisted_text_is_input_and_strangers_are_dropped() {
        let ok = route_one(
            json!({ "update_id": 1, "message": message(ALLOWED, json!({ "text": "hi" })) }),
        );
        assert_eq!(
            ok,
            Routed::Input(Inbound {
                message_id: 10,
                thread_id: Some(7),
                text: Some("hi".to_owned()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
            })
        );

        let stranger = route_one(
            json!({ "update_id": 2, "message": message(STRANGER, json!({ "text": "hi" })) }),
        );
        assert_eq!(stranger, Routed::Ignored(Ignored::NotAllowed));

        let callback = |from: i64| {
            json!({ "update_id": 3, "callback_query": {
                "id": "q1", "from": { "id": from, "is_bot": false, "first_name": "x" },
                "chat_instance": "c", "data": "allow:abcde",
                "message": message(BOT, json!({ "text": "prompt" })),
            }})
        };
        assert_eq!(
            route_one(callback(STRANGER)),
            Routed::Ignored(Ignored::NotAllowed)
        );
        assert_eq!(
            route_one(callback(ALLOWED)),
            Routed::Callback(CallbackInput {
                query_id: "q1".to_owned(),
                data: Some("allow:abcde".to_owned()),
                message_id: Some(10),
            })
        );
    }

    #[test]
    fn only_an_explicit_reply_is_a_reply() {
        let reply_to = |extra: Value| match route_one(
            json!({ "update_id": 1, "message": message(ALLOWED, extra) }),
        ) {
            Routed::Input(input) => input.reply_to,
            other => panic!("not input: {other:?}"),
        };
        // Telegram points every topic message at the topic root (id 7 here).
        assert_eq!(
            reply_to(json!({ "text": "hi", "reply_to_message": { "message_id": 7 } })),
            None
        );
        assert_eq!(
            reply_to(
                json!({ "text": "hi", "reply_to_message": { "message_id": 42, "text": "bot" } })
            ),
            Some(42)
        );
        assert_eq!(reply_to(json!({ "text": "hi" })), None);
        // General: no thread, so any replied id is explicit.
        assert_eq!(
            reply_to(
                json!({ "text": "hi", "is_topic_message": false, "message_thread_id": null,
                "reply_to_message": { "message_id": 42 } })
            ),
            Some(42)
        );
    }

    fn input(extra: Value) -> Inbound {
        match route_one(json!({ "update_id": 1, "message": message(ALLOWED, extra) })) {
            Routed::Input(input) => input,
            other => panic!("not input: {other:?}"),
        }
    }

    /// The replied message as Telegram nests it: a bot message in topic 7.
    fn replied(message_id: i64, words: Value) -> Value {
        let mut replied = json!({
            "message_id": message_id, "message_thread_id": 7, "is_topic_message": true,
            "date": 1, "from": { "id": BOT, "is_bot": true, "first_name": "bot" },
            "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
        });
        if let (Some(target), Some(words)) = (replied.as_object_mut(), words.as_object()) {
            target.extend(words.clone());
        }
        replied
    }

    #[test]
    fn an_explicit_reply_carries_the_selected_quote_or_the_start_of_the_message() {
        let selected = input(json!({
            "text": "Удаляй",
            "reply_to_message": replied(42, json!({ "text": "Удалить build/ и dist/?" })),
            "quote": { "text": "dist/", "position": 17, "is_manual": true },
        }));
        assert_eq!(selected.reply_to, Some(42));
        assert_eq!(selected.quote.as_deref(), Some("dist/"));

        let whole = input(json!({
            "text": "да",
            "reply_to_message": replied(42, json!({ "text": "Удалить build/?" })),
        }));
        assert_eq!(whole.quote.as_deref(), Some("Удалить build/?"));

        let caption = input(json!({
            "text": "да",
            "reply_to_message": replied(43, json!({
                "photo": [{ "file_id": "f", "file_unique_id": "u", "width": 1, "height": 1 }],
                "caption": "схема",
            })),
        }));
        assert_eq!(caption.quote.as_deref(), Some("схема"));

        let long = "я".repeat(QUOTE_LIMIT + 100);
        let cut = input(json!({
            "text": "да", "reply_to_message": replied(44, json!({ "text": long })),
        }))
        .quote
        .unwrap_or_default();
        assert_eq!(cut.chars().count(), QUOTE_LIMIT);
        assert!(cut.ends_with("я…"), "{cut}");

        // A replied message without words (a sticker) gives no quote.
        let bare = input(json!({
            "text": "?", "reply_to_message": replied(45, json!({ "sticker": {} })),
        }));
        assert_eq!((bare.reply_to, bare.quote), (Some(45), None));
    }

    #[test]
    fn the_implicit_reply_to_the_topic_root_carries_no_quote() {
        let root = input(json!({
            "text": "hi",
            "reply_to_message": replied(7, json!({
                "forum_topic_created": { "name": "[box] p", "icon_color": 7322096 },
            })),
        }));
        assert_eq!((root.reply_to, root.quote), (None, None));
        let plain = input(json!({ "text": "hi" }));
        assert_eq!((plain.quote, plain.forwarded), (None, false));
    }

    #[test]
    fn a_forwarded_message_is_marked() {
        for origin in [
            json!({ "type": "hidden_user", "sender_user_name": "someone", "date": 1 }),
            json!({ "type": "channel", "date": 1, "message_id": 5,
                "chat": { "id": -1009, "type": "channel", "title": "c" } }),
        ] {
            let forwarded = input(json!({ "text": "чужие слова", "forward_origin": origin }));
            assert!(forwarded.forwarded, "{forwarded:?}");
            assert_eq!(forwarded.text.as_deref(), Some("чужие слова"));
        }
    }

    #[test]
    fn a_photo_of_any_announced_dimensions_is_taken() {
        let photo = input(json!({
            "photo": [
                { "file_id": "small", "file_unique_id": "u1", "width": 90, "height": 60 },
                { "file_id": "odd", "file_unique_id": "u2", "width": u64::MAX, "height": 3 },
            ],
        }));
        assert_eq!(photo.media.unwrap().file.file_id, "odd");
    }

    #[test]
    fn a_media_message_carries_its_file_and_caption_and_no_text() {
        let photo = input(json!({
            "caption": "/brief",
            "photo": [
                { "file_id": "small", "file_unique_id": "u1", "width": 90, "height": 60, "file_size": 900 },
                { "file_id": "large", "file_unique_id": "u2", "width": 1280, "height": 853, "file_size": 90000 },
                { "file_id": "mid", "file_unique_id": "u3", "width": 320, "height": 213 },
            ],
        }));
        // A caption is never a command: the text stays empty.
        assert_eq!(photo.text, None);
        let media = photo.media.unwrap();
        assert_eq!(media.caption.as_deref(), Some("/brief"));
        assert_eq!(
            media.file,
            Attachment {
                kind: FileKind::Photo,
                file_id: "large".into(),
                name: None,
                size: Some(90000),
            }
        );
        let kind = |extra: Value| {
            input(extra)
                .media
                .map(|media| (media.file.kind, media.file.file_id))
        };
        let info = |id: &str| json!({ "file_id": id, "file_unique_id": "u", "file_name": "n.bin", "file_size": 5 });
        assert_eq!(
            kind(json!({ "document": info("d") })),
            Some((FileKind::Document, "d".into()))
        );
        assert_eq!(
            kind(json!({ "video": info("v") })),
            Some((FileKind::Video, "v".into()))
        );
        assert_eq!(
            kind(json!({ "voice": info("o") })),
            Some((FileKind::Voice, "o".into()))
        );
        assert_eq!(
            kind(json!({ "audio": info("a") })),
            Some((FileKind::Audio, "a".into()))
        );
        // Telegram fills `document` for an animation too.
        assert_eq!(
            kind(json!({ "animation": info("g"), "document": info("g") })),
            Some((FileKind::Animation, "g".into()))
        );
        let named = input(json!({ "document": info("d") })).media.unwrap().file;
        assert_eq!(
            (named.name.as_deref(), named.size),
            (Some("n.bin"), Some(5))
        );
        // Kinds the session does not take carry nothing.
        for other in [
            json!({ "sticker": { "file_id": "s" } }),
            json!({ "video_note": { "file_id": "n" } }),
            json!({ "location": { "latitude": 1.0, "longitude": 2.0 } }),
            json!({ "document": { "file_unique_id": "no id" } }),
        ] {
            let other = input(other);
            assert_eq!((other.text, other.media), (None, None));
        }
        // Text keeps its old shape.
        assert_eq!(input(json!({ "text": "hi" })).media, None);
    }

    #[test]
    fn other_chats_and_senderless_messages_are_ignored() {
        let mut other = message(ALLOWED, json!({ "text": "hi" }));
        other["chat"]["id"] = json!(-1009);
        assert_eq!(
            route_one(json!({ "update_id": 1, "message": other })),
            Routed::Ignored(Ignored::OtherChat)
        );

        let mut senderless = message(ALLOWED, json!({ "text": "hi" }));
        senderless.as_object_mut().map(|m| m.remove("from"));
        assert_eq!(
            route_one(json!({ "update_id": 1, "message": senderless })),
            Routed::Ignored(Ignored::NoSender)
        );
    }

    #[test]
    fn forum_service_messages_are_never_input() {
        let cases = [
            (
                "forum_topic_created",
                json!({ "name": "t", "icon_color": 7322096 }),
                ServiceKind::TopicCreated,
            ),
            (
                "forum_topic_edited",
                json!({ "icon_custom_emoji_id": "5" }),
                ServiceKind::TopicEdited,
            ),
            ("forum_topic_closed", json!({}), ServiceKind::TopicClosed),
            (
                "forum_topic_reopened",
                json!({}),
                ServiceKind::TopicReopened,
            ),
            (
                "pinned_message",
                json!({ "message_id": 42, "date": 1, "chat": { "id": CHAT, "type": "supergroup" } }),
                ServiceKind::Pinned(42),
            ),
        ];
        for (field, payload, kind) in cases {
            // Sent by the bot (not allowlisted) and by an allowlisted admin.
            for from in [BOT, ALLOWED] {
                let update =
                    json!({ "update_id": 1, "message": message(from, json!({ field: payload })) });
                assert_eq!(
                    route_one(update),
                    Routed::Service(ServiceMessage {
                        kind,
                        message_id: 10,
                        thread_id: Some(7),
                        from: Some(from),
                    }),
                    "{field}"
                );
            }
        }
    }

    #[test]
    fn unknown_types_fields_and_bad_shapes_do_not_stop_the_batch() {
        let batch = vec![
            json!({ "update_id": 5, "message_reaction": { "chat": { "id": CHAT } } }),
            json!({ "update_id": 6, "message": message(ALLOWED, json!({
                "text": "ok", "brand_new_field": { "nested": [1, 2] }, "rich_message": {}
            })) }),
            json!({ "update_id": 7, "message": message(ALLOWED, json!({ "text": 5 })) }),
            json!({ "no_update_id": true }),
            json!(["not", "an", "object"]),
            json!({ "update_id": 8, "message": message(ALLOWED, json!({ "text": "last" })) }),
        ];
        let (next, routed) = route_batch(batch, Some(3), CHAT, &allowlist());
        assert_eq!(next, Some(9));
        assert_eq!(routed.len(), 6);
        assert_eq!(routed[0], Routed::Ignored(Ignored::Unsupported));
        assert!(matches!(&routed[1], Routed::Input(input) if input.text.as_deref() == Some("ok")));
        assert_eq!(routed[2], Routed::Ignored(Ignored::Malformed));
        assert_eq!(routed[3], Routed::Ignored(Ignored::Unsupported));
        assert_eq!(routed[4], Routed::Ignored(Ignored::Malformed));
        assert!(
            matches!(&routed[5], Routed::Input(input) if input.text.as_deref() == Some("last"))
        );

        let (next, routed) = route_batch(Vec::new(), Some(3), CHAT, &allowlist());
        assert_eq!((next, routed.len()), (Some(3), 0));
    }

    #[test]
    fn next_offset_follows_the_batch_even_below_the_old_one() {
        // Telegram restarts ids at random after a week without updates.
        let batch = vec![json!({ "update_id": 40 }), json!({ "update_id": 42 })];
        let (next, _) = route_batch(batch, Some(9000), CHAT, &allowlist());
        assert_eq!(next, Some(43));
        let (next, _) = route_batch(vec![json!({ "x": 1 })], Some(9000), CHAT, &allowlist());
        assert_eq!(next, Some(9000));
    }

    /// Answers like Telegram: every update at or above `offset`, or waits out
    /// the long poll when there is none. It never forgets an update, which is
    /// what Telegram does for an update that was not yet confirmed.
    struct FakeTelegram(Vec<Value>);

    impl UpdateSource for FakeTelegram {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(
            &self,
            offset: Option<i64>,
            timeout: Duration,
        ) -> Result<Vec<Value>, ApiError> {
            let batch: Vec<Value> = self
                .0
                .iter()
                .filter(|update| {
                    let id = update["update_id"].as_i64().unwrap_or_default();
                    offset.is_none_or(|offset| id >= offset)
                })
                .cloned()
                .collect();
            if batch.is_empty() {
                tokio::time::sleep(timeout).await;
            }
            Ok(batch)
        }
    }

    fn text_update(id: i64, text: &str) -> Value {
        json!({ "update_id": id, "message": message(ALLOWED, json!({ "text": text })) })
    }

    /// Polls `updates` for a few simulated minutes; returns the handled texts.
    async fn handled(updates: Vec<Value>, store: &OffsetStore) -> Vec<String> {
        let mut texts = Vec::new();
        let source = FakeTelegram(updates);
        let allowlist = allowlist();
        let polling = poll(&source, &allowlist, store, |routed| {
            if let Routed::Input(input) = routed {
                texts.push(input.text.unwrap_or_default());
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(180), polling).await;
        texts
    }

    #[tokio::test(start_paused = true)]
    async fn saved_offset_prevents_handling_an_update_twice_after_restart() {
        let dir = crate::hub::testdir::TempDir::new("poll-restart");
        let first_run = OffsetStore::open(dir.path()).unwrap();
        assert_eq!(
            handled(vec![text_update(5, "/brief")], &first_run).await,
            ["/brief"]
        );

        // Restart: a new store over the same directory, and Telegram still
        // holds update 5 because no later getUpdates confirmed it.
        let restarted = OffsetStore::open(dir.path()).unwrap();
        assert_eq!(restarted.load(), Some(6));
        let pending = vec![text_update(5, "/brief"), text_update(6, "/full")];
        assert_eq!(handled(pending.clone(), &restarted).await, ["/full"]);

        // Control: without the saved offset the old command runs again.
        let empty = crate::hub::testdir::TempDir::new("poll-no-offset");
        let fresh = OffsetStore::open(empty.path()).unwrap();
        assert_eq!(handled(pending, &fresh).await, ["/brief", "/full"]);
    }

    /// Keeps returning its updates until `getUpdates` is called with an offset
    /// above them, whatever offset it was called with before: what a bot saw
    /// after a week-long pause, when Telegram restarted ids below the offset.
    struct RestartedIds(Vec<Value>, std::sync::Mutex<i64>);

    impl UpdateSource for RestartedIds {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(
            &self,
            offset: Option<i64>,
            timeout: Duration,
        ) -> Result<Vec<Value>, ApiError> {
            // Only an offset just above the update confirms it; the stale
            // 5000 does not (the observed behaviour this models).
            let confirmed = {
                let mut confirmed = self.1.lock().unwrap();
                if let Some(offset) = offset.filter(|offset| *offset < 1000) {
                    *confirmed = (*confirmed).max(offset);
                }
                *confirmed
            };
            let batch: Vec<Value> = self
                .0
                .iter()
                .filter(|update| update["update_id"].as_i64().unwrap_or_default() >= confirmed)
                .cloned()
                .collect();
            if batch.is_empty() {
                tokio::time::sleep(timeout).await;
            }
            Ok(batch)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ids_restarted_below_the_saved_offset_are_handled_once() {
        let dir = crate::hub::testdir::TempDir::new("poll-restarted-ids");
        let store = OffsetStore::open(dir.path()).unwrap();
        store.save(5000).unwrap();
        let mut texts = Vec::new();
        let source = RestartedIds(vec![text_update(7, "/brief")], std::sync::Mutex::new(0));
        let allowlist = allowlist();
        let polling = poll(&source, &allowlist, &store, |routed| {
            if let Routed::Input(input) = routed {
                texts.push(input.text.unwrap_or_default());
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(180), polling).await;
        assert_eq!(texts, ["/brief"]);
        assert_eq!(store.load(), Some(8));
    }

    #[tokio::test(start_paused = true)]
    async fn offset_is_saved_before_the_batch_is_handled() {
        let dir = crate::hub::testdir::TempDir::new("poll-save-first");
        let store = OffsetStore::open(dir.path()).unwrap();
        let mut seen = Vec::new();
        let source = FakeTelegram(vec![text_update(5, "/brief")]);
        let allowlist = allowlist();
        let polling = poll(&source, &allowlist, &store, |_| seen.push(store.load()));
        let _ = tokio::time::timeout(Duration::from_secs(180), polling).await;
        assert_eq!(seen, [Some(6)]);
    }

    /// Makes `offset` a non-empty directory, so every save fails.
    fn block_saves(dir: &std::path::Path) -> std::path::PathBuf {
        let blocker = dir.join("offset");
        std::fs::create_dir_all(blocker.join("inside")).unwrap();
        blocker
    }

    #[tokio::test(start_paused = true)]
    async fn failing_offset_saves_do_not_stop_polling() {
        let dir = crate::hub::testdir::TempDir::new("poll-save-fails");
        let store = OffsetStore::open(dir.path()).unwrap();
        block_saves(dir.path());
        let updates = vec![text_update(5, "/brief"), text_update(6, "/full")];
        // Both handled once: the in-memory offset still moves on.
        assert_eq!(handled(updates, &store).await, ["/brief", "/full"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_briefly_failing_offset_save_is_retried() {
        let dir = crate::hub::testdir::TempDir::new("poll-save-retry");
        let store = OffsetStore::open(dir.path()).unwrap();
        let blocker = block_saves(dir.path());
        // Unblocked after the first attempt, before the retry 100 ms later.
        let unblock = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::remove_dir_all(blocker).unwrap();
        });
        assert_eq!(
            handled(vec![text_update(5, "/brief")], &store).await,
            ["/brief"]
        );
        unblock.await.unwrap();
        assert_eq!(store.load(), Some(6));
    }

    #[test]
    fn nonempty_batch_without_update_ids_gets_a_short_backoff() {
        assert_eq!(
            stalled_batch_backoff(2, Some(3), Some(3)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(stalled_batch_backoff(0, Some(3), Some(3)), None);
        assert_eq!(stalled_batch_backoff(2, Some(3), Some(4)), None);
    }
}

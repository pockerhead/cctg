//! Inbound side: long polling, allowlist gate, service-message classification.
//!
//! `Routed` values carry no Telegram user id, so nothing downstream can log one.

use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use super::api::{ApiError, BotApi, Message, Update};
use super::config::Allowlist;

const POLL_TIMEOUT: Duration = Duration::from_secs(50);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    TopicCreated,
    TopicEdited,
    TopicClosed,
    TopicReopened,
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
        None
    }
}

/// Classifies one parsed update. Service messages are recognised before the
/// allowlist check because the bot itself is their sender.
pub fn classify(update: Update, chat_id: i64, allowlist: &Allowlist) -> Routed {
    if let Some(message) = update.message {
        if message.chat.id != chat_id {
            return Routed::Ignored(Ignored::OtherChat);
        }
        if let Some(kind) = service_kind(&message) {
            return Routed::Service(ServiceMessage {
                kind,
                message_id: message.message_id,
                thread_id: message.message_thread_id,
            });
        }
        let Some(from) = message.from else {
            return Routed::Ignored(Ignored::NoSender);
        };
        if !allowlist.contains(from.id) {
            return Routed::Ignored(Ignored::NotAllowed);
        }
        return Routed::Input(Inbound {
            message_id: message.message_id,
            thread_id: message
                .message_thread_id
                .filter(|_| message.is_topic_message),
            text: message.text,
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
pub fn route_batch(
    raw: Vec<Value>,
    offset: Option<i64>,
    chat_id: i64,
    allowlist: &Allowlist,
) -> (Option<i64>, Vec<Routed>) {
    let mut next = offset;
    let mut routed = Vec::with_capacity(raw.len());
    for value in raw {
        if let Some(id) = value.get("update_id").and_then(Value::as_i64) {
            next = Some(next.map_or(id + 1, |current| current.max(id + 1)));
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
    (next, routed)
}

/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
pub async fn poll(api: &BotApi, allowlist: &Allowlist, mut handle: impl FnMut(Routed)) {
    let mut offset = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        match api.get_updates(offset, POLL_TIMEOUT).await {
            Ok(raw) => {
                backoff = Duration::from_secs(1);
                let (next, routed) = route_batch(raw, offset, api.chat_id(), allowlist);
                offset = next;
                routed.into_iter().for_each(&mut handle);
            }
            Err(ApiError::RetryAfter(wait)) => {
                warn!(?wait, "getUpdates hit flood control");
                tokio::time::sleep(wait).await;
            }
            Err(error) => {
                warn!(%error, ?backoff, "getUpdates failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

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
                text: Some("hi".to_owned())
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
                        thread_id: Some(7)
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

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Ok(mut out) = self.0.lock() {
                out.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn routing_logs_never_contain_user_ids() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let batch = vec![
                json!({ "update_id": 1, "message": message(STRANGER, json!({ "text": "x" })) }),
                json!({ "update_id": 2, "message": message(ALLOWED, json!({ "text": "x" })) }),
                json!({ "update_id": 3, "message": message(BOT, json!({ "forum_topic_closed": {} })) }),
                json!({ "update_id": 4, "message": message(STRANGER, json!({ "text": 1 })) }),
            ];
            route_batch(batch, None, CHAT, &allowlist());
        });
        let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
            .unwrap_or_default();
        assert!(
            logs.contains("update ignored"),
            "expected debug logs: {logs}"
        );
        for id in [STRANGER, ALLOWED, BOT] {
            assert!(!logs.contains(&id.to_string()), "user id in logs: {logs}");
        }
    }
}

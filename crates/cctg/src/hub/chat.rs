//! The chats the hub writes to (TASK-061).
//!
//! Telegram numbers messages and topics per chat, so a message or a topic is
//! known only together with its chat: [`MessageKey`] and [`Place`]. The
//! forum group is [`Chat::Group`]; its id lives in [`super::api::BotApi`]
//! only. A private chat of the bot with a user has that user's id, so
//! [`PrivateChat`] never prints it: not in `Debug`, not in logs, meta or
//! Telegram texts. It is kept only in memory and in `registry.json`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The private chat of the bot with one user; its id is the user's id.
/// `Debug` never prints it and there is no `Display`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrivateChat(i64);

impl PrivateChat {
    /// The private chat of user `user_id` (its chat id equals the user id).
    pub fn of_user(user_id: i64) -> Self {
        Self(user_id)
    }

    /// The chat id, for the Bot API request body only.
    pub(crate) fn expose(self) -> i64 {
        self.0
    }
}

impl fmt::Debug for PrivateChat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateChat(..)")
    }
}

/// A chat the hub talks in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chat {
    /// The forum supergroup of `CCTG_CHAT_ID`.
    Group,
    /// The bot's private chat with a user.
    Private(PrivateChat),
}

impl Chat {
    /// `group` or `private`: the place label the session sees in the meta of
    /// a message (`place`), never an id.
    pub fn label(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Private(_) => "private",
        }
    }

    pub fn is_private(self) -> bool {
        matches!(self, Self::Private(_))
    }
}

impl fmt::Debug for Chat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Group => f.write_str("Group"),
            Self::Private(_) => f.write_str("Private(..)"),
        }
    }
}

/// A chat and a topic in it; `thread: None` is its General (no topic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Place {
    pub chat: Chat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<i64>,
}

impl Place {
    pub fn new(chat: Chat, thread: Option<i64>) -> Self {
        Self { chat, thread }
    }

    /// Topic `thread` of `chat`.
    pub fn topic(chat: Chat, thread: i64) -> Self {
        Self {
            chat,
            thread: Some(thread),
        }
    }
}

/// A message of a chat: message ids are numbered per chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageKey {
    pub chat: Chat,
    pub id: i64,
}

impl MessageKey {
    pub fn new(chat: Chat, id: i64) -> Self {
        Self { chat, id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinctive: it must not show up by chance.
    const USER: i64 = 7_319_402_518;

    #[test]
    fn a_private_chat_never_prints_its_id() {
        let chat = Chat::Private(PrivateChat::of_user(USER));
        let place = Place::topic(chat, 42);
        let key = MessageKey::new(chat, 5);
        let shown = format!(
            "{chat:?} {place:?} {key:?} {:?} {:#?}",
            PrivateChat::of_user(USER),
            place
        );
        assert!(!shown.contains(&USER.to_string()), "{shown}");
        assert!(shown.contains("Private(..)"), "{shown}");
        assert_eq!(chat.label(), "private");
        assert_eq!(Chat::Group.label(), "group");
        assert_eq!(format!("{:?}", Chat::Group), "Group");
    }

    #[test]
    fn chats_places_and_keys_serialize_plainly() {
        let group = serde_json::to_value(MessageKey::new(Chat::Group, 5)).unwrap();
        assert_eq!(group, serde_json::json!({"chat": "group", "id": 5}));
        let private = Chat::Private(PrivateChat::of_user(12));
        assert_eq!(
            serde_json::to_value(Place::topic(private, 7)).unwrap(),
            serde_json::json!({"chat": {"private": 12}, "thread": 7})
        );
        assert_eq!(
            serde_json::to_value(Place::new(Chat::Group, None)).unwrap(),
            serde_json::json!({"chat": "group"})
        );
        let back: Place =
            serde_json::from_value(serde_json::json!({"chat": {"private": 12}, "thread": 7}))
                .unwrap();
        assert_eq!(back, Place::topic(private, 7));
        assert_eq!(PrivateChat::of_user(12).expose(), 12);
    }

    #[test]
    fn the_same_id_in_two_chats_is_two_messages() {
        let private = Chat::Private(PrivateChat::of_user(USER));
        assert_ne!(MessageKey::new(Chat::Group, 5), MessageKey::new(private, 5));
        assert_ne!(Place::topic(Chat::Group, 5), Place::topic(private, 5));
        assert_ne!(
            Chat::Private(PrivateChat::of_user(1)),
            Chat::Private(PrivateChat::of_user(2))
        );
    }
}

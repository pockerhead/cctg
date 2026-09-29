//! Inbound side: long polling, allowlist gate, service-message classification.
//!
//! `Routed` values carry no Telegram user id, so nothing downstream can log one.
//! A private chat is a [`Chat::Private`], whose id never prints; so is a
//! group's ([`GroupChat`]). Messages and presses come only from the groups
//! the hub knows ([`KnownGroups`], TASK-069); from another group only the
//! bot's own membership changes (`my_chat_member`) and `/connect` of an
//! allowlisted user get through.

use std::future::Future;
use std::io;
use std::pin::pin;
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use super::api::{
    ApiError, BotApi, ChatMember, FileInfo, Message, MessageChat, MessageOrigin, Update, User,
};
use super::buffer::Attachment;
use super::chat::{Chat, GroupChat, MessageKey, Place, PrivateChat};
use super::config::Allowlist;
use super::groups::KnownGroups;
use super::offset::OffsetStore;
use super::people;
use super::registry::cut;
use crate::wire::FileKind;

const POLL_TIMEOUT: Duration = Duration::from_secs(50);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STALLED_BATCH_BACKOFF: Duration = Duration::from_secs(1);
/// UTF-16 units kept of a replied message quoted for the session.
pub const QUOTE_LIMIT: usize = 500;
/// Nesting of a rich message's blocks read for a quote, at most (TASK-075).
const RICH_DEPTH: usize = 32;
/// Waits before the second and third attempt to save the offset (a file held
/// open by a scanner or indexer on Windows makes the rename fail for a moment).
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// A message from an allowlisted user in the group (or, once private
    /// chats are on, in an allowlisted user's private chat).
    Input(Inbound),
    /// A button press from an allowlisted user.
    Callback(CallbackInput),
    /// A forum service message. Never user input; the hub may delete it.
    Service(ServiceMessage),
    /// The bot's membership in a group changed (`my_chat_member`,
    /// TASK-069), known group or not.
    Member(MemberUpdate),
    /// `/connect` from an allowlisted user in a group, known or not
    /// (TASK-069).
    Connect(ConnectInput),
    /// A forward in the General of an allowlisted user's private chat
    /// (TASK-081): the person to add.
    Forward(ForwardInput),
    /// `/start inv_<code>` of someone not allowlisted in their private chat
    /// with the bot (TASK-081).
    Invite(InviteInput),
    Ignored(Ignored),
}

/// A forward in the General of `chat` (TASK-081). Never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardInput {
    pub chat: PrivateChat,
    pub message_id: i64,
    pub origin: Origin,
}

/// Who wrote a forwarded message first (TASK-081).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A user Telegram names: their private chat, their [`display_name`]
    /// and cleaned username.
    User {
        user: PrivateChat,
        is_bot: bool,
        name: Option<String>,
        username: Option<String>,
    },
    /// A user who hides their account in forwards.
    Hidden,
    /// A chat, a channel, or a kind the hub does not know.
    Other,
}

/// `/start inv_<code>` from `user` (TASK-081). Never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteInput {
    pub user: PrivateChat,
    pub code: InviteCode,
    pub name: Option<String>,
    pub username: Option<String>,
}

/// An invite code without its prefix. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteCode(String);

impl InviteCode {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for InviteCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InviteCode(..)")
    }
}

/// The bot's new membership in a group (TASK-069). Who changed it is only
/// `by_allowed` and `by`; neither is ever logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberUpdate {
    pub chat: GroupChat,
    /// A supergroup, not a plain group.
    pub supergroup: bool,
    /// Its title ([`group_title`]); never logged.
    pub title: Option<String>,
    pub is_forum: bool,
    pub member: ChatMember,
    /// An allowlisted user made the change.
    pub by_allowed: bool,
    /// That allowlisted user (TASK-081: one removed meanwhile does not
    /// count). Never logged.
    pub by: Option<PrivateChat>,
}

/// `/connect` in a group (TASK-069).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectInput {
    pub chat: GroupChat,
    pub supergroup: bool,
    pub title: Option<String>,
    pub is_forum: bool,
    /// Its topic, as [`Inbound::thread_id`].
    pub thread_id: Option<i64>,
    /// The bot `/connect@<name>` names; `None` for a bare `/connect`.
    pub target: Option<String>,
    /// Who sent it (TASK-081: one removed meanwhile is dropped). Never
    /// logged.
    pub sender: PrivateChat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    /// The chat of the message: every id here is one of this chat.
    pub chat: Chat,
    /// Who wrote it, as the private chat with them (TASK-063: `/join`
    /// makes its sender the owner of the device it enrolls). Never logged.
    pub sender: PrivateChat,
    pub message_id: i64,
    /// `None` for the General topic.
    pub thread_id: Option<i64>,
    pub text: Option<String>,
    /// The message this one explicitly answers. `None` for the implicit
    /// reply to the topic root that Telegram sets on every topic message.
    pub reply_to: Option<i64>,
    /// The sender id of the message [`Inbound::reply_to`] answers, when
    /// Telegram gave it: a reply to the bot's own message addresses the
    /// agent (TASK-077). Never logged.
    pub reply_from: Option<i64>,
    /// The words an explicit reply answers: the fragment the user selected,
    /// else the start of the replied text or caption, at most
    /// [`QUOTE_LIMIT`]. Never logged.
    pub quote: Option<String>,
    /// A forwarded message: someone else's words, not the user's.
    pub forwarded: bool,
    /// A file the message carried and its caption (TASK-032); `text` is
    /// then `None`. Never logged but for its kind and size.
    pub media: Option<Media>,
    /// The sender's [`author_name`], only when the allowlist is a team
    /// (TASK-036). Never logged.
    pub from_name: Option<String>,
    /// The sender's [`author_name`] always: it signs the echo of the message
    /// in the slot's other views (TASK-063). Never logged.
    pub author: Option<String>,
    /// The sender's [`display_name`]: it signs the share line of `/share`
    /// (TASK-064). Never logged.
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub file: Attachment,
    pub caption: Option<String>,
    /// Telegram's `media_group_id`: the album of the file (TASK-077).
    /// Never logged.
    pub album: Option<String>,
    /// Seconds of a voice message, as its sender set it (TASK-085).
    pub duration: Option<u64>,
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
    let duration = message.media.voice.as_ref().and_then(|info| info.duration);
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
        duration: duration.filter(|_| file.kind == FileKind::Voice),
        file,
        caption: message.media.caption.take(),
        album: message.media.media_group_id.take(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackInput {
    pub query_id: String,
    pub data: Option<String>,
    /// The chat of the pressed message; `None` when Telegram did not say.
    pub chat: Option<Chat>,
    pub message_id: Option<i64>,
    /// The topic of the pressed message, as [`Inbound::thread_id`]: `None`
    /// in General or when Telegram did not say.
    pub thread_id: Option<i64>,
    /// Who pressed, as [`Inbound::from_name`].
    pub from_name: Option<String>,
    /// Who pressed, as [`Inbound::display_name`]: it signs the share line
    /// (TASK-064). Never logged.
    pub display_name: Option<String>,
    /// Who pressed, as the private chat with them (TASK-081: only owners
    /// manage people and devices). Never logged.
    pub sender: PrivateChat,
}

/// UTF-16 units kept of an author's name.
pub const NAME_LIMIT: usize = 32;
/// UTF-16 units kept of a group's title (TASK-069).
pub const GROUP_TITLE_LIMIT: usize = 64;

/// How a team member is shown to the session and in signed answers: the
/// username, else the first name. Line breaks and other whitespace become
/// one space; control characters, invisible formatting ones (bidi
/// overrides, zero-width) and `<>"` are dropped; at most [`NAME_LIMIT`].
/// `None` when nothing is left. Never the user id.
pub fn author_name(from: &User) -> Option<String> {
    from.username
        .as_deref()
        .and_then(|name| clean_name(name, NAME_LIMIT))
        .or_else(|| {
            from.first_name
                .as_deref()
                .and_then(|name| clean_name(name, NAME_LIMIT))
        })
}

/// How a person is named in a share line (TASK-064): the first and last
/// name, else the username; cleaned and bounded as [`author_name`].
pub fn display_name(from: &User) -> Option<String> {
    let name: Vec<&str> = [from.first_name.as_deref(), from.last_name.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    clean_name(&name.join(" "), NAME_LIMIT).or_else(|| {
        from.username
            .as_deref()
            .and_then(|name| clean_name(name, NAME_LIMIT))
    })
}

/// A group's title as the hub shows it (TASK-069): cleaned like
/// [`author_name`], at most [`GROUP_TITLE_LIMIT`].
pub fn group_title(raw: &str) -> Option<String> {
    clean_name(raw, GROUP_TITLE_LIMIT)
}

/// [`author_name`]'s cleaning of one name, at most `limit` UTF-16 units.
fn clean_name(raw: &str, limit: usize) -> Option<String> {
    let invisible = |c: char| {
        matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{FEFF}'
            | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}')
    };
    let kept: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| !c.is_control() && !invisible(c) && !matches!(c, '<' | '>' | '"'))
        .collect();
    let name = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then(|| cut(&name, limit))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceMessage {
    pub chat: Chat,
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
    /// A chat the hub does not know: no known group, no allowlisted user's
    /// private chat.
    OtherChat,
    /// An allowlisted user's private chat while the hub serves none (the
    /// bot has no topics in private chats, TASK-063).
    PrivateChat,
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

/// The chat of a message: a known group, or the private chat of an
/// allowlisted user (a private chat's id is its user's id). Anything else is
/// no chat of the hub.
fn chat_of(chat: &MessageChat, groups: &KnownGroups, allowlist: &Allowlist) -> Option<Chat> {
    if chat.kind != "private" && groups.contains(chat.id) {
        return Some(Chat::Group(GroupChat::of(chat.id)));
    }
    (chat.kind == "private" && allowlist.contains(chat.id))
        .then(|| Chat::Private(PrivateChat::of_user(chat.id)))
}

/// A group or supergroup chat (TASK-069).
fn is_group(chat: &MessageChat) -> bool {
    matches!(chat.kind.as_str(), "group" | "supergroup")
}

/// The bot a `/connect` text names: `Some(None)` for a bare `/connect`,
/// `Some(Some(name))` for `/connect@name`; `None` for any other text.
fn connect_target(text: &str) -> Option<Option<String>> {
    let word = text.split_whitespace().next()?;
    let (command, target) = match word.split_once('@') {
        Some((command, target)) => (command, Some(target.to_owned())),
        None => (word, None),
    };
    command.eq_ignore_ascii_case("/connect").then_some(target)
}

/// `/connect` of an allowlisted user in a group, known or not (TASK-069).
fn connect_of(message: &Message, allowlist: &Allowlist) -> Option<ConnectInput> {
    if !is_group(&message.chat) || message.forward_origin.is_some() {
        return None;
    }
    let target = connect_target(message.text.as_deref()?)?;
    let sender = message
        .from
        .as_ref()
        .filter(|from| allowlist.contains(from.id))?
        .id;
    Some(ConnectInput {
        chat: GroupChat::of(message.chat.id),
        supergroup: message.chat.kind == "supergroup",
        title: message.chat.title.as_deref().and_then(group_title),
        is_forum: message.chat.is_forum,
        thread_id: message
            .message_thread_id
            .filter(|_| message.is_topic_message),
        target,
        sender: PrivateChat::of_user(sender),
    })
}

/// Who first wrote a forwarded message (TASK-081). A `user` origin without
/// a real user id is no user.
fn origin_of(origin: &MessageOrigin) -> Origin {
    match origin.kind.as_str() {
        "user" => match &origin.sender_user {
            Some(user) if user.id > 0 => Origin::User {
                user: PrivateChat::of_user(user.id),
                is_bot: user.is_bot,
                name: display_name(user),
                username: user
                    .username
                    .as_deref()
                    .and_then(|name| clean_name(name, NAME_LIMIT)),
            },
            _ => Origin::Other,
        },
        "hidden_user" => Origin::Hidden,
        _ => Origin::Other,
    }
}

/// `/start inv_<code>` (TASK-081): someone not allowlisted, no bot, in their
/// own private chat with the bot, typed (not forwarded), exactly two words.
fn invite_of(message: &Message, allowlist: &Allowlist) -> Option<InviteInput> {
    let from = message.from.as_ref()?;
    if message.chat.kind != "private"
        || from.id != message.chat.id
        || from.is_bot
        || allowlist.contains(from.id)
        || message.forward_origin.is_some()
    {
        return None;
    }
    let mut words = message.text.as_deref()?.split_whitespace();
    let (Some("/start"), Some(payload), None) = (words.next(), words.next(), words.next()) else {
        return None;
    };
    if !people::is_invite_payload(payload) {
        return None;
    }
    Some(InviteInput {
        user: PrivateChat::of_user(from.id),
        code: InviteCode(payload[people::INVITE_PREFIX.len()..].to_owned()),
        name: display_name(from),
        username: from
            .username
            .as_deref()
            .and_then(|name| clean_name(name, NAME_LIMIT)),
    })
}

/// Classifies one parsed update. Service messages are recognised before the
/// allowlist check because the bot itself is their sender.
pub fn classify(update: Update, groups: &KnownGroups, allowlist: &Allowlist) -> Routed {
    if let Some(mut message) = update.message {
        if let Some(connect) = connect_of(&message, allowlist) {
            return Routed::Connect(connect);
        }
        if let Some(invite) = invite_of(&message, allowlist) {
            return Routed::Invite(invite);
        }
        let Some(chat) = chat_of(&message.chat, groups, allowlist) else {
            return Routed::Ignored(Ignored::OtherChat);
        };
        if let Some(kind) = service_kind(&message) {
            return Routed::Service(ServiceMessage {
                chat,
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
        let author = author_name(&from);
        let from_name = author.clone().filter(|_| allowlist.is_team());
        let display_name = display_name(&from);
        let media = media(&mut message);
        let thread_id = message
            .message_thread_id
            .filter(|_| message.is_topic_message);
        // A forward in the General of a private chat names a person to add
        // (TASK-081); in a topic or a group it is a message as any other.
        if let (Chat::Private(private), None, Some(origin)) =
            (chat, thread_id, message.forward_origin.as_deref())
        {
            return Routed::Forward(ForwardInput {
                chat: private,
                message_id: message.message_id,
                origin: origin_of(origin),
            });
        }
        let replied = message
            .reply_to_message
            .filter(|replied| replied.message_id != 0 && Some(replied.message_id) != thread_id);
        let reply_to = replied.as_ref().map(|replied| replied.message_id);
        let reply_from = replied
            .as_ref()
            .and_then(|replied| replied.from)
            .map(|from| from.id);
        let quote = replied.and_then(|replied| {
            let replied = *replied;
            let words = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
            words(message.quote.map(|quote| quote.text))
                .or_else(|| words(replied.text))
                .or_else(|| words(replied.caption))
                // A rich message has no `text` (TASK-075).
                .or_else(|| words(replied.rich_message.as_deref().map(rich_words)))
                .map(|text| cut(&text, QUOTE_LIMIT))
        });
        return Routed::Input(Inbound {
            chat,
            sender: PrivateChat::of_user(from.id),
            message_id: message.message_id,
            thread_id,
            text: message.text,
            reply_to,
            reply_from,
            quote,
            forwarded: message.forward_origin.is_some(),
            media,
            from_name,
            author,
            display_name,
        });
    }

    if let Some(query) = update.callback_query {
        let chat = match &query.message {
            Some(message) => match chat_of(&message.chat, groups, allowlist) {
                Some(chat) => Some(chat),
                None => return Routed::Ignored(Ignored::OtherChat),
            },
            None => None,
        };
        let Some(from) = query.from else {
            return Routed::Ignored(Ignored::NoSender);
        };
        if !allowlist.contains(from.id) {
            return Routed::Ignored(Ignored::NotAllowed);
        }
        let thread_id = query.message.as_ref().and_then(|message| {
            message
                .message_thread_id
                .filter(|_| message.is_topic_message)
        });
        return Routed::Callback(CallbackInput {
            query_id: query.id,
            data: query.data,
            chat,
            message_id: query.message.map(|message| message.message_id),
            thread_id,
            from_name: author_name(&from).filter(|_| allowlist.is_team()),
            display_name: display_name(&from),
            sender: PrivateChat::of_user(from.id),
        });
    }

    if let Some(changed) = update.my_chat_member {
        let chat = &changed.chat;
        if chat.kind == "private" {
            return Routed::Ignored(Ignored::Unsupported);
        }
        if !is_group(chat) {
            return Routed::Ignored(Ignored::OtherChat);
        }
        let by = changed
            .from
            .as_ref()
            .filter(|from| allowlist.contains(from.id))
            .map(|from| PrivateChat::of_user(from.id));
        return Routed::Member(MemberUpdate {
            chat: GroupChat::of(chat.id),
            supergroup: chat.kind == "supergroup",
            title: chat.title.as_deref().and_then(group_title),
            is_forum: chat.is_forum,
            member: changed.new_chat_member,
            by_allowed: by.is_some(),
            by,
        });
    }

    Routed::Ignored(Ignored::Unsupported)
}

/// Without topics in private chats the hub serves only the group:
/// everything from a private chat is ignored, as before TASK-061.
fn group_only(routed: Routed) -> Routed {
    let private = match &routed {
        Routed::Input(input) => input.chat.is_private(),
        Routed::Callback(input) => input.chat.is_some_and(Chat::is_private),
        Routed::Service(service) => service.chat.is_private(),
        Routed::Forward(_) | Routed::Invite(_) => true,
        Routed::Member(_) | Routed::Connect(_) | Routed::Ignored(_) => false,
    };
    if private {
        Routed::Ignored(Ignored::PrivateChat)
    } else {
        routed
    }
}

impl Inbound {
    /// Where the message was written.
    pub fn place(&self) -> Place {
        Place::new(self.chat, self.thread_id)
    }

    pub fn key(&self) -> MessageKey {
        MessageKey::new(self.chat, self.message_id)
    }
}

impl CallbackInput {
    /// The pressed message, when Telegram named it.
    pub fn message(&self) -> Option<MessageKey> {
        Some(MessageKey::new(self.chat?, self.message_id?))
    }

    /// The topic of the pressed message, when Telegram named it.
    pub fn topic(&self) -> Option<Place> {
        Some(Place::topic(self.chat?, self.thread_id?))
    }
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
    groups: &KnownGroups,
    allowlist: &Allowlist,
) -> (Option<i64>, Vec<Routed>) {
    route_batch_with(raw, offset, groups, allowlist, false)
}

/// [`route_batch`]; `private`: the private chats of allowlisted users are
/// served too (TASK-063, the bot has topics there).
pub fn route_batch_with(
    raw: Vec<Value>,
    offset: Option<i64>,
    groups: &KnownGroups,
    allowlist: &Allowlist,
    private: bool,
) -> (Option<i64>, Vec<Routed>) {
    let mut highest: Option<i64> = None;
    let mut routed = Vec::with_capacity(raw.len());
    for value in raw {
        if let Some(id) = value.get("update_id").and_then(Value::as_i64) {
            highest = Some(highest.map_or(id, |current| current.max(id)));
        }
        let item = match serde_json::from_value::<Update>(value) {
            Ok(update) if private => classify(update, groups, allowlist),
            Ok(update) => group_only(classify(update, groups, allowlist)),
            Err(_) => Routed::Ignored(Ignored::Malformed),
        };
        match &item {
            Routed::Ignored(reason) => debug!(?reason, "update ignored"),
            Routed::Service(service) => debug!(kind = ?service.kind, "forum service message"),
            Routed::Input(_)
            | Routed::Callback(_)
            | Routed::Member(_)
            | Routed::Connect(_)
            | Routed::Forward(_)
            | Routed::Invite(_) => {}
        }
        routed.push(item);
    }
    let next = highest.map(|id| id.saturating_add(1)).or(offset);
    (next, routed)
}

/// Where updates come from: `BotApi` in production, a fake in tests.
pub trait UpdateSource {
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<Value>, ApiError>> + Send;
}

impl UpdateSource for BotApi {
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
    groups: &KnownGroups,
    allowlist: &Allowlist,
    store: &OffsetStore,
    handle: impl FnMut(Routed),
) {
    poll_until(
        source,
        groups,
        allowlist,
        false,
        store,
        handle,
        std::future::pending(),
    )
    .await;
}

/// [`poll`] until `stop` completes. It is only noticed while no batch is
/// being handled: a batch whose offset was saved is always handed out whole,
/// and an interrupted `getUpdates` confirms nothing, so its updates come again.
/// `private`: as in [`route_batch_with`]; `groups`: read for each batch, so
/// a group the actor adds counts from the next one.
pub async fn poll_until<S: UpdateSource>(
    source: &S,
    groups: &KnownGroups,
    allowlist: &Allowlist,
    private: bool,
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
                let (next, routed) = route_batch_with(raw, offset, groups, allowlist, private);
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

/// The words of a rich message (TASK-075) for a reply quote: the text of
/// its blocks in order, one line each (a table row as its cells joined by
/// ` | `), blank ones left out. Its `text` is a string, or inline parts
/// (strings and objects with a `text` of their own) in an array.
fn rich_words(rich: &Value) -> String {
    let mut lines = Vec::new();
    rich_lines(rich, 0, &mut lines);
    lines.join("\n")
}

fn rich_lines(value: &Value, depth: usize, lines: &mut Vec<String>) {
    if depth > RICH_DEPTH {
        return;
    }
    match value {
        Value::Array(blocks) => {
            for block in blocks {
                rich_lines(block, depth + 1, lines);
            }
        }
        Value::Object(block) => {
            let mut push = |line: String| {
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            };
            push(inline_words(value, depth));
            if let Some(Value::Array(rows)) = block.get("cells") {
                for row in rows {
                    let cells: Vec<String> = row
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|cell| inline_words(cell, depth + 1))
                        .collect();
                    push(cells.join(" | "));
                }
            }
            for key in ["blocks", "items"] {
                if let Some(nested) = block.get(key) {
                    rich_lines(nested, depth + 1, lines);
                }
            }
        }
        _ => {}
    }
}

/// The inline text of a block or part: its `text` (a formula's
/// `expression`), recursively.
fn inline_words(value: &Value, depth: usize) -> String {
    if depth > RICH_DEPTH {
        return String::new();
    }
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| inline_words(part, depth + 1))
            .collect(),
        Value::Object(part) => match (part.get("text"), part.get("expression")) {
            (Some(text), _) => inline_words(text, depth + 1),
            (None, Some(Value::String(expression))) => expression.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CHAT: i64 = -1000000000001;
    const GROUP: Chat = Chat::Group(GroupChat::of(CHAT));
    const ALLOWED: i64 = 1001;
    const STRANGER: i64 = 2002;
    const BOT: i64 = 3003;

    fn allowlist() -> Allowlist {
        [ALLOWED].into_iter().collect()
    }

    fn groups() -> KnownGroups {
        KnownGroups::of([GroupChat::of(CHAT)])
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
        let (_, mut routed) = route_batch(vec![update], None, &groups(), &allowlist());
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
                chat: GROUP,
                sender: PrivateChat::of_user(ALLOWED),
                message_id: 10,
                thread_id: Some(7),
                text: Some("hi".to_owned()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
                from_name: None,
                author: Some("x".to_owned()),
                display_name: Some("x".to_owned()),
                reply_from: None,
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
                chat: Some(GROUP),
                query_id: "q1".to_owned(),
                data: Some("allow:abcde".to_owned()),
                message_id: Some(10),
                thread_id: Some(7),
                from_name: None,
                display_name: Some("x".to_owned()),
                sender: PrivateChat::of_user(ALLOWED),
            })
        );
    }

    #[test]
    fn a_team_names_the_author_of_messages_and_presses_and_one_person_does_not() {
        const MATE: i64 = 1002;
        let team: Allowlist = [ALLOWED, MATE].into_iter().collect();
        let route = |update: Value, allowlist: &Allowlist| {
            let (_, mut routed) = route_batch(vec![update], None, &groups(), allowlist);
            routed.remove(0)
        };
        let from =
            json!({ "id": MATE, "is_bot": false, "first_name": "Анна", "username": "anna_k" });
        let mut text = message(MATE, json!({ "text": "hi" }));
        text["from"] = from.clone();
        let press = json!({ "update_id": 2, "callback_query": {
            "id": "q1", "from": from, "chat_instance": "c", "data": "allow:abcde",
            "message": message(BOT, json!({ "text": "prompt" })),
        }});
        let text = json!({ "update_id": 1, "message": text });
        match route(text.clone(), &team) {
            Routed::Input(input) => assert_eq!(input.from_name.as_deref(), Some("anna_k")),
            other => panic!("{other:?}"),
        }
        match route(press.clone(), &team) {
            Routed::Callback(input) => assert_eq!(input.from_name.as_deref(), Some("anna_k")),
            other => panic!("{other:?}"),
        }
        // One person: no names, as before the team mode.
        let alone: Allowlist = [MATE].into_iter().collect();
        assert!(matches!(
            route(text.clone(), &alone),
            Routed::Input(Inbound {
                from_name: None,

                ..
            })
        ));
        // The name, not the username, signs a share line either way
        // (TASK-064).
        assert!(matches!(
            route(press, &alone),
            Routed::Callback(CallbackInput {
                from_name: None,
                display_name: Some(ref name),
                ..
            }) if name == "Анна"
        ));
        assert!(matches!(
            route(text, &alone),
            Routed::Input(Inbound {
                author: Some(ref author),
                display_name: Some(ref name),
                ..
            }) if author == "anna_k" && name == "Анна"
        ));
    }

    #[test]
    fn a_display_name_is_the_name_else_the_username() {
        let user = |username: Option<&str>, first: Option<&str>, last: Option<&str>| User {
            id: 987654321,
            is_bot: false,
            username: username.map(str::to_owned),
            first_name: first.map(str::to_owned),
            last_name: last.map(str::to_owned),
            ..User::default()
        };
        assert_eq!(
            display_name(&user(Some("anna_k"), Some("Анна"), Some("Кузнецова"))).as_deref(),
            Some("Анна Кузнецова")
        );
        assert_eq!(
            display_name(&user(Some("anna_k"), None, Some("Кузнецова"))).as_deref(),
            Some("Кузнецова")
        );
        assert_eq!(
            display_name(&user(Some("anna_k"), Some("\u{200B}"), None)).as_deref(),
            Some("anna_k")
        );
        assert_eq!(
            display_name(&user(None, Some(" Иван\n"), Some("<Петров>"))).as_deref(),
            Some("Иван Петров")
        );
        assert_eq!(display_name(&user(None, None, None)), None);
    }

    #[test]
    fn an_author_name_is_cleaned_and_bounded() {
        let user = |username: Option<&str>, first_name: Option<&str>| User {
            id: 987654321,
            is_bot: false,
            username: username.map(str::to_owned),
            first_name: first_name.map(str::to_owned),
            ..User::default()
        };
        assert_eq!(
            author_name(&user(Some("anna_k"), Some("Анна"))).as_deref(),
            Some("anna_k")
        );
        assert_eq!(
            author_name(&user(None, Some("Анна"))).as_deref(),
            Some("Анна")
        );
        // Line breaks, tabs, controls, bidi overrides, zero-width and tag
        // characters go; runs of spaces become one.
        assert_eq!(
            author_name(&user(
                None,
                Some("  Иван\n\tПетров\u{0007}\u{202E}\u{200B} <b>\"x\"  ")
            ))
            .as_deref(),
            Some("Иван Петров bx")
        );
        // Nothing visible left: the next choice, else no name at all.
        assert_eq!(
            author_name(&user(Some("\u{200B}"), Some("Анна"))).as_deref(),
            Some("Анна")
        );
        assert_eq!(author_name(&user(Some(" "), Some("\n"))), None);
        assert_eq!(author_name(&user(None, None)), None);
        let long = author_name(&user(None, Some(&"😀".repeat(100)))).unwrap();
        assert!(transcript::telegram_len(&long) <= NAME_LIMIT, "{long}");
        assert!(long.ends_with('…'));
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

    /// TASK-077: the sender of an explicitly replied message is kept by id;
    /// the implicit reply to the topic root (the bot made it) has none.
    #[test]
    fn an_explicit_reply_keeps_the_replied_senders_id() {
        let reply_from = |extra: Value| input(extra).reply_from;
        assert_eq!(
            reply_from(json!({ "text": "hi", "reply_to_message": {
                "message_id": 42, "text": "t", "from": { "id": 7, "is_bot": false, "first_name": "a" } } })),
            Some(7)
        );
        assert_eq!(
            reply_from(json!({ "text": "hi", "reply_to_message": {
                "message_id": 7, "from": { "id": BOT, "is_bot": true, "first_name": "bot" } } })),
            None,
            "the topic root"
        );
        assert_eq!(
            reply_from(json!({ "text": "hi", "reply_to_message": { "message_id": 42 } })),
            None
        );
        assert_eq!(reply_from(json!({ "text": "hi" })), None);
        assert_eq!(
            reply_from(json!({ "text": "hi", "reply_to_message": replied(42, json!({})) })),
            Some(BOT)
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

    /// TASK-075: a rich message has no `text`; its quote comes from its
    /// blocks (the shape of probe R1), a selected fragment still first.
    #[test]
    fn a_reply_to_a_rich_message_quotes_its_blocks() {
        let rich = json!({ "blocks": [
            { "type": "heading", "text": "Итог", "size": 1 },
            { "type": "paragraph", "text": ["Абзац с ", { "type": "bold", "text": "жирным" },
                " и ", { "type": "url", "text": "ссылкой", "url": "https://example.com" }, "."] },
            { "type": "table", "cells": [
                [{ "text": "Файл", "is_header": true }, { "text": "Строк", "is_header": true }],
                [{ "text": { "type": "code", "text": "api.rs" } }, { "text": "1109" }],
            ] },
            { "type": "list", "items": [{ "label": "1.", "blocks": [
                { "type": "paragraph", "text": "Первый" },
                { "type": "list", "items": [{ "label": "•", "blocks": [
                    { "type": "paragraph", "text": "вложенный" },
                ] }] },
            ] }] },
            { "type": "mathematical_expression", "expression": "x^2" },
            { "type": "divider" },
        ] });
        let quoted = input(json!({
            "text": "да",
            "reply_to_message": replied(46, json!({ "rich_message": rich })),
        }));
        assert_eq!(quoted.reply_to, Some(46));
        assert_eq!(
            quoted.quote.as_deref(),
            Some(
                "Итог\nАбзац с жирным и ссылкой.\nФайл | Строк\napi.rs | 1109\nПервый\nвложенный\nx^2"
            )
        );

        let long =
            json!({ "blocks": [{ "type": "paragraph", "text": "я".repeat(QUOTE_LIMIT + 100) }] });
        let cut = input(json!({
            "text": "да", "reply_to_message": replied(47, json!({ "rich_message": long })),
        }))
        .quote
        .unwrap_or_default();
        assert_eq!(cut.chars().count(), QUOTE_LIMIT);

        let selected = input(json!({
            "text": "да",
            "reply_to_message": replied(48, json!({ "rich_message": rich })),
            "quote": { "text": "жирным", "position": 8, "is_manual": true },
        }));
        assert_eq!(selected.quote.as_deref(), Some("жирным"));
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
        assert_eq!(media.album, None);
        let album = input(json!({
            "media_group_id": "13579",
            "photo": [{ "file_id": "p", "file_unique_id": "u", "width": 1, "height": 1 }],
        }));
        assert_eq!(album.media.unwrap().album.as_deref(), Some("13579"));
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
        // TASK-085: a voice message's length, as its sender set it.
        let voice = input(json!({
            "voice": { "file_id": "o", "file_unique_id": "u", "duration": 42 },
        }));
        assert_eq!(voice.media.unwrap().duration, Some(42));
        assert_eq!(media.duration, None);
        let audio = input(json!({
            "audio": { "file_id": "a", "file_unique_id": "u", "duration": 42 },
        }));
        assert_eq!(audio.media.unwrap().duration, None);
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

    /// TASK-061: a private chat of an allowlisted user is recognised (its
    /// chat id is the user id) but ignored until TASK-063 serves it; another
    /// user's private chat is no chat of the hub.
    #[test]
    fn a_private_chat_is_known_but_not_served_yet() {
        let private = |from: i64, extra: Value| {
            let mut message = message(from, extra);
            message["chat"] = json!({ "id": from, "type": "private", "first_name": "x" });
            message
        };
        let text = private(ALLOWED, json!({ "text": "hi" }));
        let classified = classify(
            serde_json::from_value(json!({ "update_id": 1, "message": text.clone() })).unwrap(),
            &groups(),
            &allowlist(),
        );
        let Routed::Input(input) = &classified else {
            panic!("{classified:?}");
        };
        assert_eq!(input.chat, Chat::Private(PrivateChat::of_user(ALLOWED)));
        assert_eq!(input.key(), MessageKey::new(input.chat, 10));
        assert!(!format!("{classified:?}").contains(&ALLOWED.to_string()));
        assert_eq!(
            route_one(json!({ "update_id": 1, "message": text })),
            Routed::Ignored(Ignored::PrivateChat)
        );
        let press = json!({ "update_id": 2, "callback_query": {
            "id": "q1", "from": { "id": ALLOWED, "is_bot": false, "first_name": "x" },
            "chat_instance": "c", "data": "allow:abcde",
            "message": private(ALLOWED, json!({ "text": "prompt" })),
        }});
        assert_eq!(route_one(press), Routed::Ignored(Ignored::PrivateChat));
        // The bot edits a topic of the owner's private chat.
        let mut service = private(ALLOWED, json!({ "forum_topic_edited": {} }));
        service["from"]["id"] = json!(BOT);
        assert_eq!(
            route_one(json!({ "update_id": 3, "message": service })),
            Routed::Ignored(Ignored::PrivateChat)
        );
        let stranger = private(STRANGER, json!({ "text": "hi" }));
        assert_eq!(
            route_one(json!({ "update_id": 4, "message": stranger })),
            Routed::Ignored(Ignored::OtherChat)
        );
        // A group whose id happens to be an allowlisted user's is no private chat.
        let mut group = message(ALLOWED, json!({ "text": "hi" }));
        group["chat"]["id"] = json!(ALLOWED);
        assert_eq!(
            route_one(json!({ "update_id": 5, "message": group })),
            Routed::Ignored(Ignored::OtherChat)
        );
    }

    /// TASK-063: with topics in private chats the hub serves the private
    /// chat of an allowlisted user: its messages, presses and service
    /// messages go on with the chat, and nobody else's private chat does.
    #[test]
    fn a_private_chat_is_served_when_the_bot_has_topics_there() {
        let route = |update: Value| {
            let (_, mut routed) =
                route_batch_with(vec![update], None, &groups(), &allowlist(), true);
            routed.remove(0)
        };
        let private = |from: i64, extra: Value| {
            let mut message = message(from, extra);
            message["chat"] = json!({ "id": from, "type": "private", "first_name": "x" });
            message
        };
        let owner = Chat::Private(PrivateChat::of_user(ALLOWED));
        let text =
            route(json!({ "update_id": 1, "message": private(ALLOWED, json!({ "text": "hi" })) }));
        assert!(
            matches!(&text, Routed::Input(Inbound { chat, thread_id: Some(7), .. }) if *chat == owner),
            "{text:?}"
        );
        let press = route(json!({ "update_id": 2, "callback_query": {
            "id": "q1", "from": { "id": ALLOWED, "is_bot": false, "first_name": "x" },
            "chat_instance": "c", "data": "allow:abcde",
            "message": private(ALLOWED, json!({ "text": "prompt" })),
        }}));
        assert!(
            matches!(&press, Routed::Callback(CallbackInput { chat: Some(chat), .. }) if *chat == owner),
            "{press:?}"
        );
        let mut service = private(ALLOWED, json!({ "forum_topic_edited": {} }));
        service["from"]["id"] = json!(BOT);
        assert!(matches!(
            route(json!({ "update_id": 3, "message": service })),
            Routed::Service(ServiceMessage {
                kind: ServiceKind::TopicEdited,
                ..
            })
        ));
        assert_eq!(
            route(json!({ "update_id": 4, "message": private(STRANGER, json!({ "text": "hi" })) })),
            Routed::Ignored(Ignored::OtherChat)
        );
    }

    /// TASK-073: a menu press of someone not allowlisted, or on a message of
    /// someone else's private chat, never reaches the slot actor.
    #[test]
    fn a_strangers_menu_press_is_ignored() {
        let route = |update: Value| {
            let (_, mut routed) =
                route_batch_with(vec![update], None, &groups(), &allowlist(), true);
            routed.remove(0)
        };
        let private = |of: i64| {
            let mut message = message(of, json!({ "text": "menu" }));
            message["chat"] = json!({ "id": of, "type": "private", "first_name": "x" });
            message
        };
        let press = |from: i64, of: i64| {
            json!({ "update_id": 2, "callback_query": {
                "id": "q1", "from": { "id": from, "is_bot": false, "first_name": "x" },
                "chat_instance": "c", "data": "menu:dl:a",
                "message": private(of),
            }})
        };
        assert_eq!(
            route(press(STRANGER, STRANGER)),
            Routed::Ignored(Ignored::OtherChat)
        );
        assert_eq!(
            route(press(STRANGER, ALLOWED)),
            Routed::Ignored(Ignored::NotAllowed)
        );
        let owner = Chat::Private(PrivateChat::of_user(ALLOWED));
        let own = route(press(ALLOWED, ALLOWED));
        assert!(
            matches!(&own, Routed::Callback(CallbackInput { chat: Some(chat), data: Some(data), .. })
                if *chat == owner && data == "menu:dl:a"),
            "{own:?}"
        );
    }

    /// TASK-081: a forward in the General of an allowlisted user's private
    /// chat names a person to add; anywhere else it is a message.
    #[test]
    fn a_forward_in_a_private_general_names_its_author() {
        let route = |update: Value, private: bool| {
            let (_, mut routed) =
                route_batch_with(vec![update], None, &groups(), &allowlist(), private);
            routed.remove(0)
        };
        let general = |origin: Value| {
            let mut message = message(
                ALLOWED,
                json!({ "text": "их слова", "forward_origin": origin }),
            );
            message["chat"] = json!({ "id": ALLOWED, "type": "private", "first_name": "x" });
            message.as_object_mut().unwrap().remove("message_thread_id");
            message["is_topic_message"] = json!(false);
            json!({ "update_id": 1, "message": message })
        };
        let owner = PrivateChat::of_user(ALLOWED);
        let user = json!({ "type": "user", "date": 1, "sender_user": {
            "id": STRANGER, "is_bot": false, "first_name": "Анна", "last_name": "К",
            "username": "anna\u{202E}" } });
        assert_eq!(
            route(general(user.clone()), true),
            Routed::Forward(ForwardInput {
                chat: owner,
                message_id: 10,
                origin: Origin::User {
                    user: PrivateChat::of_user(STRANGER),
                    is_bot: false,
                    name: Some("Анна К".into()),
                    username: Some("anna".into()),
                },
            })
        );
        let bot = json!({ "type": "user", "date": 1, "sender_user": {
            "id": BOT, "is_bot": true, "first_name": "b" } });
        assert!(matches!(
            route(general(bot), true),
            Routed::Forward(ForwardInput {
                origin: Origin::User { is_bot: true, .. },
                ..
            })
        ));
        for (origin, want) in [
            (
                json!({ "type": "hidden_user", "date": 1, "sender_user_name": "Анна" }),
                Origin::Hidden,
            ),
            (json!({ "type": "hidden_user", "date": 1 }), Origin::Hidden),
            (
                json!({ "type": "chat", "date": 1, "sender_chat": { "id": CHAT, "type": "supergroup" } }),
                Origin::Other,
            ),
            (json!({ "type": "channel", "date": 1 }), Origin::Other),
            (json!({ "type": "user", "date": 1 }), Origin::Other),
            (
                json!({ "type": "user", "date": 1, "sender_user": { "first_name": "x" } }),
                Origin::Other,
            ),
        ] {
            assert_eq!(
                route(general(origin.clone()), true),
                Routed::Forward(ForwardInput {
                    chat: owner,
                    message_id: 10,
                    origin: want,
                }),
                "{origin}"
            );
        }
        // Without topics in private chats the private chat is not served.
        assert_eq!(
            route(general(user.clone()), false),
            Routed::Ignored(Ignored::PrivateChat)
        );
        // In a topic of the private chat and in the group: a forwarded message.
        let mut topic = message(
            ALLOWED,
            json!({ "text": "t", "forward_origin": user.clone() }),
        );
        topic["chat"] = json!({ "id": ALLOWED, "type": "private", "first_name": "x" });
        assert!(matches!(
            route(json!({ "update_id": 2, "message": topic }), true),
            Routed::Input(Inbound {
                forwarded: true,
                thread_id: Some(7),
                ..
            })
        ));
        let group = message(ALLOWED, json!({ "text": "t", "forward_origin": user }));
        assert!(matches!(
            route(json!({ "update_id": 3, "message": group }), true),
            Routed::Input(Inbound {
                forwarded: true,
                ..
            })
        ));
    }

    /// TASK-081: `/start inv_<code>` of someone not allowlisted in their own
    /// private chat is an invite; nothing else of a stranger is.
    #[test]
    fn only_a_typed_start_with_an_invite_code_of_a_stranger_is_an_invite() {
        const CODE: &str = "AbCdEfGhIjKlMnOpQr-_09";
        let route = |update: Value, private: bool| {
            let (_, mut routed) =
                route_batch_with(vec![update], None, &groups(), &allowlist(), private);
            routed.remove(0)
        };
        let start = |from: i64, text: &str| {
            let mut message = message(from, json!({ "text": text }));
            message["chat"] = json!({ "id": from, "type": "private", "first_name": "x" });
            message["from"]["username"] = json!("anna");
            json!({ "update_id": 1, "message": message })
        };
        let invite = route(start(STRANGER, &format!("/start inv_{CODE}")), true);
        let Routed::Invite(input) = &invite else {
            panic!("{invite:?}");
        };
        assert_eq!(input.user, PrivateChat::of_user(STRANGER));
        assert_eq!(input.code.as_str(), CODE);
        assert_eq!(input.name.as_deref(), Some("x"));
        assert_eq!(input.username.as_deref(), Some("anna"));
        let shown = format!("{invite:?}");
        assert!(
            !shown.contains(CODE) && !shown.contains(&STRANGER.to_string()),
            "{shown}"
        );
        // Topics are off: the private chat is not served.
        assert_eq!(
            route(start(STRANGER, &format!("/start inv_{CODE}")), false),
            Routed::Ignored(Ignored::PrivateChat)
        );
        for text in [
            "/start".to_owned(),
            "/start inv_short".to_owned(),
            "/start inv_AbCdEfGhIjKlMnOpQr-_0!".to_owned(),
            format!("/start inv_{CODE} extra"),
            format!("/menu inv_{CODE}"),
        ] {
            assert_eq!(
                route(start(STRANGER, &text), true),
                Routed::Ignored(Ignored::OtherChat),
                "{text}"
            );
        }
        // An allowlisted user's is their message.
        assert!(matches!(
            route(start(ALLOWED, &format!("/start inv_{CODE}")), true),
            Routed::Input(_)
        ));
        // In a group, from a bot, forwarded: nothing.
        let group = message(STRANGER, json!({ "text": format!("/start inv_{CODE}") }));
        assert_eq!(
            route(json!({ "update_id": 2, "message": group }), true),
            Routed::Ignored(Ignored::NotAllowed)
        );
        let mut bot = start(STRANGER, &format!("/start inv_{CODE}"));
        bot["message"]["from"]["is_bot"] = json!(true);
        assert_eq!(route(bot, true), Routed::Ignored(Ignored::OtherChat));
        let mut forwarded = start(STRANGER, &format!("/start inv_{CODE}"));
        forwarded["message"]["forward_origin"] = json!({ "type": "hidden_user", "date": 1 });
        assert_eq!(route(forwarded, true), Routed::Ignored(Ignored::OtherChat));
    }

    /// TASK-081: a member added at run time writes from the next update on,
    /// named as in a team; removed, they are dropped again.
    #[test]
    fn members_count_from_the_next_update() {
        let allowlist = allowlist();
        let route = |allowlist: &Allowlist| {
            let update =
                json!({ "update_id": 1, "message": message(STRANGER, json!({ "text": "hi" })) });
            let (_, mut routed) = route_batch(vec![update], None, &groups(), allowlist);
            routed.remove(0)
        };
        assert_eq!(route(&allowlist), Routed::Ignored(Ignored::NotAllowed));
        allowlist.set_members([PrivateChat::of_user(STRANGER)]);
        assert!(matches!(
            route(&allowlist),
            Routed::Input(Inbound { from_name: Some(ref name), .. }) if name == "x"
        ));
        let press = json!({ "update_id": 2, "callback_query": {
            "id": "q1", "from": { "id": STRANGER, "is_bot": false, "first_name": "x" },
            "chat_instance": "c", "data": "allow:abcde",
            "message": message(BOT, json!({ "text": "prompt" })),
        }});
        let (_, mut routed) = route_batch(vec![press.clone()], None, &groups(), &allowlist);
        assert!(matches!(
            routed.remove(0),
            Routed::Callback(CallbackInput { sender, .. }) if sender == PrivateChat::of_user(STRANGER)
        ));
        allowlist.set_members([]);
        assert_eq!(route(&allowlist), Routed::Ignored(Ignored::NotAllowed));
        let (_, mut routed) = route_batch(vec![press], None, &groups(), &allowlist);
        assert_eq!(routed.remove(0), Routed::Ignored(Ignored::NotAllowed));
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
                        chat: GROUP,
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
        let (next, routed) = route_batch(batch, Some(3), &groups(), &allowlist());
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

        let (next, routed) = route_batch(Vec::new(), Some(3), &groups(), &allowlist());
        assert_eq!((next, routed.len()), (Some(3), 0));
    }

    #[test]
    fn next_offset_follows_the_batch_even_below_the_old_one() {
        // Telegram restarts ids at random after a week without updates.
        let batch = vec![json!({ "update_id": 40 }), json!({ "update_id": 42 })];
        let (next, _) = route_batch(batch, Some(9000), &groups(), &allowlist());
        assert_eq!(next, Some(43));
        let (next, _) = route_batch(vec![json!({ "x": 1 })], Some(9000), &groups(), &allowlist());
        assert_eq!(next, Some(9000));
    }

    /// Answers like Telegram: every update at or above `offset`, or waits out
    /// the long poll when there is none. It never forgets an update, which is
    /// what Telegram does for an update that was not yet confirmed.
    struct FakeTelegram(Vec<Value>);

    impl UpdateSource for FakeTelegram {
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
        let groups = groups();
        let polling = poll(&source, &groups, &allowlist, store, |routed| {
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
        let groups = groups();
        let polling = poll(&source, &groups, &allowlist, &store, |routed| {
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
        let groups = groups();
        let polling = poll(&source, &groups, &allowlist, &store, |_| {
            seen.push(store.load())
        });
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

    /// TASK-069: messages, presses and service messages count only from a
    /// known group; another group is `OtherChat`.
    #[test]
    fn only_known_groups_get_through() {
        const OTHER: i64 = -1000000000002;
        let known = KnownGroups::of([GroupChat::of(CHAT), GroupChat::of(OTHER)]);
        let in_chat = |id: i64, extra: Value| {
            let mut message = message(ALLOWED, extra);
            message["chat"]["id"] = json!(id);
            message
        };
        let route = |update: Value, groups: &KnownGroups| {
            let (_, mut routed) = route_batch(vec![update], None, groups, &allowlist());
            routed.remove(0)
        };
        let text = json!({ "update_id": 1, "message": in_chat(OTHER, json!({ "text": "hi" })) });
        match route(text.clone(), &known) {
            Routed::Input(input) => {
                assert_eq!(input.chat, Chat::Group(GroupChat::of(OTHER)));
                assert_ne!(input.chat, GROUP);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(route(text, &groups()), Routed::Ignored(Ignored::OtherChat));
        let press = json!({ "update_id": 2, "callback_query": {
            "id": "q1", "from": { "id": ALLOWED, "is_bot": false, "first_name": "x" },
            "chat_instance": "c", "data": "allow:abcde",
            "message": in_chat(OTHER, json!({ "text": "prompt" })),
        }});
        assert_eq!(route(press, &groups()), Routed::Ignored(Ignored::OtherChat));
        let service = json!({ "update_id": 3,
            "message": in_chat(OTHER, json!({ "forum_topic_edited": { "name": "x" } })) });
        assert_eq!(
            route(service, &groups()),
            Routed::Ignored(Ignored::OtherChat)
        );
    }

    fn member_update(chat: Value, from: i64, status: &str) -> Value {
        json!({ "update_id": 1, "my_chat_member": {
            "chat": chat,
            "from": { "id": from, "is_bot": false, "first_name": "x" },
            "date": 1,
            "old_chat_member": { "status": "left", "user": { "id": BOT, "is_bot": true, "first_name": "b" } },
            "new_chat_member": { "status": status, "user": { "id": BOT, "is_bot": true, "first_name": "b" },
                "can_manage_topics": true, "can_delete_messages": true },
        }})
    }

    /// TASK-069: the bot's membership changes of any group reach the actor
    /// with whether an allowlisted user made them; a private chat's (a
    /// block) and a channel's do not.
    #[test]
    fn membership_changes_of_any_group_get_through() {
        const NEW: i64 = -1000000000009;
        let group = json!({ "id": NEW, "type": "supergroup", "title": "Команда\n\u{202E}x", "is_forum": true });
        let Routed::Member(added) =
            route_one(member_update(group.clone(), ALLOWED, "administrator"))
        else {
            panic!("not a member update");
        };
        assert_eq!(added.chat, GroupChat::of(NEW));
        assert!(added.supergroup && added.is_forum && added.by_allowed);
        assert_eq!(added.title.as_deref(), Some("Команда x"));
        assert_eq!(added.member.status, "administrator");
        assert!(added.member.can_manage_topics);
        let Routed::Member(by_stranger) = route_one(member_update(group, STRANGER, "member"))
        else {
            panic!("not a member update");
        };
        assert!(!by_stranger.by_allowed);
        let basic = json!({ "id": NEW, "type": "group", "title": "g" });
        let Routed::Member(basic) = route_one(member_update(basic, ALLOWED, "member")) else {
            panic!("not a member update");
        };
        assert!(!basic.supergroup && !basic.is_forum);
        let private = json!({ "id": ALLOWED, "type": "private", "first_name": "x" });
        assert_eq!(
            route_one(member_update(private, ALLOWED, "kicked")),
            Routed::Ignored(Ignored::Unsupported)
        );
        let channel = json!({ "id": NEW, "type": "channel", "title": "c" });
        assert_eq!(
            route_one(member_update(channel, ALLOWED, "administrator")),
            Routed::Ignored(Ignored::OtherChat)
        );
    }

    /// TASK-069: `/connect` of an allowlisted user in any group is its own
    /// input, also in a topic of a known group; a stranger's is dropped
    /// like any message there; in a private chat it is a plain message.
    #[test]
    fn connect_comes_from_allowlisted_users_in_any_group() {
        const NEW: i64 = -1000000000009;
        let connect = |from: i64, chat: Value, text: &str| {
            let mut message = message(from, json!({ "text": text }));
            message["chat"] = chat;
            json!({ "update_id": 1, "message": message })
        };
        let unknown =
            json!({ "id": NEW, "type": "supergroup", "title": "Новая", "is_forum": false });
        assert_eq!(
            route_one(connect(STRANGER, unknown.clone(), "/connect")),
            Routed::Ignored(Ignored::OtherChat)
        );
        assert_eq!(
            route_one(connect(ALLOWED, unknown.clone(), "/connect")),
            Routed::Connect(ConnectInput {
                chat: GroupChat::of(NEW),
                supergroup: true,
                title: Some("Новая".to_owned()),
                is_forum: false,
                thread_id: Some(7),
                target: None,
                sender: PrivateChat::of_user(ALLOWED),
            })
        );
        match route_one(connect(ALLOWED, unknown, "/CONNECT@Other_Bot please")) {
            Routed::Connect(input) => assert_eq!(input.target.as_deref(), Some("Other_Bot")),
            other => panic!("{other:?}"),
        }
        let known = json!({ "id": CHAT, "type": "supergroup", "is_forum": true });
        match route_one(connect(ALLOWED, known.clone(), "/connect@cctg_bot")) {
            Routed::Connect(input) => {
                assert_eq!(input.chat, GroupChat::of(CHAT));
                assert_eq!(input.thread_id, Some(7));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            route_one(connect(STRANGER, known.clone(), "/connect")),
            Routed::Ignored(Ignored::NotAllowed)
        );
        assert!(matches!(
            route_one(connect(ALLOWED, known, "/connected")),
            Routed::Input(_)
        ));
        let private = json!({ "id": ALLOWED, "type": "private", "first_name": "x" });
        let (_, mut routed) = route_batch_with(
            vec![connect(ALLOWED, private, "/connect")],
            None,
            &groups(),
            &allowlist(),
            true,
        );
        assert!(matches!(routed.remove(0), Routed::Input(_)));
    }

    #[test]
    fn a_group_title_is_cleaned_and_bounded() {
        let long = "Группа ".repeat(20);
        let title = group_title(&long).unwrap();
        assert!(
            transcript::telegram_len(&title) <= GROUP_TITLE_LIMIT,
            "{title}"
        );
        assert!(title.ends_with('…'));
        assert_eq!(group_title("a\u{0000}b\n c").as_deref(), Some("ab c"));
        assert_eq!(group_title(" \u{200B} "), None);
    }
}

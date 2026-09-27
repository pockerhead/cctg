//! Outbound scheduler: every Bot API write goes through one queue.
//!
//! Queues, checked in this order on every pick:
//! 1. a permission prompt from `Message` that has no older message of its topic
//!    queued (stream lines do not count); metered. With a debounce, the
//!    `merge` lines of its topic queued before it go first, at once, as one
//!    message where they fit (they happened before the prompt).
//! 2. `answerCallbackQuery` from `Edit`; no token.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`,
//!    `unpinChatMessage` - and foreground `Edit` - `editMessageText` and
//!    `setMessageReaction` that show what a user did or asked for
//!    (decisions, questions, subagent blocks, reactions, ⏹). When both
//!    wait they take turns, so neither waits behind more than one of the
//!    other, and neither ever waits behind a background edit.
//!    A due stream message that turns a status message into turn content
//!    (`Op::Stream::into`, not `merge`, TASK-062) counts as a foreground
//!    edit.
//! 4. background `Edit` - the periodic refresh of a status message
//!    (`Op::Edit::background`) - and due stream messages that grow a turn
//!    message (`Op::Stream::into`, `merge`, TASK-062), the one queued
//!    longest first: only the edit budget foreground edits left.
//!    Edits and reactions are coalesced per message (the newest text wins,
//!    in the oldest one's place, foreground when either was), so the
//!    background queue holds one edit per message and is served oldest
//!    first: round-robin over the status messages, one slot cannot hog it.
//! 5. `Message` - `sendMessage`, `sendDocument`, `sendPhoto`, `sendMediaGroup`
//!    (one token per album) and transcript stream lines; metered, one FIFO. Permission prompts live here too, so they never
//!    overtake their own topic's ordinary messages; they do overtake its
//!    stream lines, except its debounced ones (1.). A new status message of
//!    their topic still queued ([`Outbox::submit_status`]) never holds them
//!    back: it is answered `Superseded` when the prompt comes (TASK-062).
//!
//! Stream lines marked `merge` (one tool call each) are debounced (TASK-054):
//! the first line of a topic waits until no further mergeable line of that
//! topic came for `Limits::debounce`, at most `Limits::debounce_max` after it
//! was handed over, and then takes the lines of its topic queued right after
//! it (up to the first other message of that topic) into one message, in
//! order, while it fits Telegram's limit. A queued message of the topic that
//! cannot join (an answer, a prompt, a loud line after quiet ones) ends the
//! wait at once. A waiting line holds back only its own topic; permission
//! prompts never wait. Without a debounce, lines go one per message while the
//! group budget has room and merge only when more messages wait than there
//! are tokens.
//!
//! In the `Topic` queue a `createForumTopic` goes first; the other topic
//! calls, deletes included, keep their order (TASK-062).
//!
//! A stream message written into an existing message (`into`, TASK-062)
//! waits in the `Message` lane and is debounced like a line, in order with
//! the other writes into that message only: it changes what is above, so it
//! neither waits for the new messages of its topic nor holds them back (a
//! new status message does not wait behind the turn message growing). It
//! takes an edit token, or a message token while no edit token is there
//! (turn lines were new messages before TASK-062: the message budget keeps
//! room for them). The `merge` ones queued after it for the same
//! message carry its text and more, so they replace its text and are
//! answered `Merged`. It also supersedes an `Edit` of that message still
//! queued (a status refresh must not overwrite the content).
//!
//! A stream line Telegram does not take (any error but a 4xx, which the
//! stream skips) breaks its topic's stream: the lines of that topic queued
//! after it, and those that come later, are answered unsent until a line
//! marked `restart` comes. So a later line never shows before the one the
//! stream sends again.
//!
//! A message with `html` goes out as Telegram HTML. When Telegram cannot parse
//! it (`400 can't parse entities`), the same job goes again once, at the head
//! of its lane, as its plain `text` without the markup.
//!
//! A ready ordinary message is served after a bounded run of unmetered jobs,
//! while a ready permission prompt always remains first.
//!
//! Metered ops (new messages) take a token from the group message bucket.
//! Edits, reactions and topic mutations have no published limit, but they
//! count against the same group (429s were seen live with them unbounded):
//! each takes a token from a second bucket, `Limits::edits`. On top of both,
//! every request but a callback answer takes a token of one budget for the
//! whole group, `Limits::group` (TASK-068: with only the two, the group saw
//! up to 40 requests a minute and a 429 about every minute). Inside it the
//! order above holds, except that a status refresh takes only what the new
//! messages and growing turn messages leave (those go oldest first), and
//! one after every four other requests while it waits.
//! Callback answers go to the pressing user, not into the group, and take no
//! token: they go ahead of edits that wait for theirs. Under a steady load of
//! background refreshes a lone topic call or foreground edit waits at most
//! for the next edit token (two when the other class waits too).
//! Everything is serialized (one request in flight).
//! Any 429 pauses the whole queue for `retry_after` and puts the job back at
//! the head of its lane; an edit or reaction whose message got a newer one
//! queued meanwhile is answered `Superseded` instead. It also halves the
//! group's rate (down to a quarter), which comes back by a tenth of the full
//! rate a minute: a real limit below the guess costs a 429 now and then, not
//! one a minute.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use super::api::{ApiError, BotApi, Document, ForumTopic, Message};
use super::chat::{Chat, MessageKey, Place};

const QUEUE_CAPACITY: usize = 1024;
const MAX_CONSECUTIVE_UNMETERED: usize = 4;
const MIN_RETRY_AFTER: Duration = Duration::from_secs(1);
/// The group's rate after 429s is at least this share of `Limits::group`.
const SLOWEST_RATE: f64 = 0.25;
/// Share of the full rate the group's rate gains back per minute: from half
/// to full in five minutes.
const RECOVERY_PER_MINUTE: f64 = 0.1;

/// Every op but a callback answer names its chat (TASK-061): topics and
/// messages are numbered per chat.
#[derive(Debug, Clone)]
pub enum Op {
    Send {
        chat: Chat,
        thread_id: Option<i64>,
        /// Plain text; the fallback when `html` is set and refused.
        text: String,
        /// `text` as Telegram HTML (`parse_mode: HTML`), sent instead of it.
        html: Option<String>,
        reply_markup: Option<Value>,
        /// Permission prompts jump ahead of ordinary messages of other topics,
        /// never ahead of older messages of their own topic but a status
        /// message, which they supersede (TASK-062).
        permission: bool,
        /// The message this one answers (`reply_parameters`).
        reply_to: Option<i64>,
        /// With a sound; `false` sends it with `disable_notification`.
        notify: bool,
    },
    SendDocument {
        chat: Chat,
        thread_id: Option<i64>,
        document: Document,
        /// As in `Send`.
        notify: bool,
    },
    /// `sendPhoto` (TASK-032); a picture Telegram refuses as a photo (400:
    /// dimensions, format) goes as a document in the same job.
    SendPhoto {
        chat: Chat,
        thread_id: Option<i64>,
        document: Document,
        /// As in `Send`.
        notify: bool,
    },
    /// `sendMediaGroup` (TASK-059): 2-10 files as one album, one message
    /// token. `photos`: a photo album; when Telegram refuses a picture of
    /// it (the whole album fails), the same files go again as a document
    /// album in the same job, as `SendPhoto` does for one picture.
    SendAlbum {
        chat: Chat,
        thread_id: Option<i64>,
        items: Vec<Document>,
        photos: bool,
        /// As in `Send`.
        notify: bool,
    },
    Edit {
        chat: Chat,
        message_id: i64,
        text: String,
        reply_markup: Option<Value>,
        /// The periodic refresh of a status message: it takes only the edit
        /// budget that every other edit left. Everything else a user does
        /// or waits for is foreground.
        background: bool,
    },
    AnswerCallback {
        query_id: String,
        text: Option<String>,
    },
    Delete {
        chat: Chat,
        message_id: i64,
    },
    /// `unpinChatMessage`: a status message a hub pinned before TASK-062
    /// that could not be deleted.
    Unpin {
        chat: Chat,
        message_id: i64,
    },
    CreateTopic {
        chat: Chat,
        name: String,
        icon_custom_emoji_id: Option<String>,
    },
    EditTopic {
        chat: Chat,
        thread_id: i64,
        name: Option<String>,
        icon_custom_emoji_id: Option<String>,
    },
    /// A message of the live transcript stream (TASK-016). `merge`: a one-line
    /// tool call that may share a message with the lines queued after it.
    /// `restart`: the first line of a stream (again); it ends a break of its
    /// topic's stream.
    Stream {
        chat: Chat,
        thread_id: i64,
        text: String,
        /// As in `Send`.
        html: Option<String>,
        merge: bool,
        restart: bool,
        /// As in `Send`; only lines of equal `notify` share a message.
        notify: bool,
        /// TASK-062: the message of the topic this is written into
        /// (`editMessageText` without buttons) instead of a new message: the
        /// status message turning into turn content, or the turn message
        /// growing. `text` is then that message's whole new text, and a
        /// later `merge` line into the same message replaces it. It takes a
        /// token of the edit budget, not of the message one, and answers
        /// `Sent` with that message's id.
        into: Option<i64>,
    },
    /// `setMessageReaction` with one emoji; a newer one for the same message
    /// replaces a queued one.
    React {
        chat: Chat,
        message_id: i64,
        emoji: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// Index of the job to send in the `Edit` queue.
    Edit(usize),
    Topic,
    /// Index of the job to send in the `Message` queue.
    Message(usize),
}

impl Op {
    fn lane(&self) -> Lane {
        match self {
            Op::Send { .. }
            | Op::SendDocument { .. }
            | Op::SendPhoto { .. }
            | Op::SendAlbum { .. }
            | Op::Stream { .. } => Lane::Message(0),
            Op::Edit { .. } | Op::AnswerCallback { .. } | Op::React { .. } => Lane::Edit(0),
            Op::Delete { .. }
            | Op::Unpin { .. }
            | Op::CreateTopic { .. }
            | Op::EditTopic { .. } => Lane::Topic,
        }
    }

    /// Only new messages count against the group message limit.
    fn metered(&self) -> bool {
        matches!(
            self,
            Op::Send { .. }
                | Op::SendDocument { .. }
                | Op::SendPhoto { .. }
                | Op::SendAlbum { .. }
                | Op::Stream { into: None, .. }
        )
    }

    /// The topic this op puts a new message into (TASK-062): every send, a
    /// stream message not written into an existing one.
    pub fn posts(&self) -> Option<Place> {
        self.metered().then(|| self.place()).flatten()
    }

    /// The order a `Message` job keeps: new messages of a topic go in the
    /// order they came, and so do the writes into one existing message
    /// (TASK-062). A write into a message above waits for no new message
    /// and holds none back: it changes what is above either way.
    fn queue(&self) -> (Option<Place>, Option<i64>) {
        let into = match self {
            Op::Stream { into, .. } => *into,
            _ => None,
        };
        (self.place(), into)
    }

    /// Requests into the group other than new messages: they take a token
    /// from the edit bucket. A callback answer goes to the user who pressed.
    fn edit_metered(&self) -> bool {
        !self.metered() && !matches!(self, Op::AnswerCallback { .. })
    }

    fn background(&self) -> bool {
        matches!(
            self,
            Op::Edit {
                background: true,
                ..
            }
        )
    }

    /// An edit or a reaction of the same message as `other`: the newer one
    /// replaces the older one.
    fn replaces(&self, other: &Op) -> bool {
        match (self, other) {
            (Op::Edit { .. }, Op::Edit { .. }) | (Op::React { .. }, Op::React { .. }) => {
                self.message().is_some() && self.message() == other.message()
            }
            _ => false,
        }
    }

    /// The message an edit or a reaction is for.
    fn message(&self) -> Option<MessageKey> {
        match self {
            Op::Edit {
                chat, message_id, ..
            }
            | Op::React {
                chat, message_id, ..
            } => Some(MessageKey::new(*chat, *message_id)),
            _ => None,
        }
    }

    /// The chat and topic of a new message.
    fn place(&self) -> Option<Place> {
        match self {
            Op::Send {
                chat, thread_id, ..
            }
            | Op::SendDocument {
                chat, thread_id, ..
            }
            | Op::SendPhoto {
                chat, thread_id, ..
            }
            | Op::SendAlbum {
                chat, thread_id, ..
            } => Some(Place::new(*chat, *thread_id)),
            Op::Stream {
                chat, thread_id, ..
            } => Some(Place::topic(*chat, *thread_id)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Sent(Message),
    Topic(ForumTopic),
    Done,
    /// A newer edit of the same message replaced this one before it was sent.
    Superseded,
    /// This stream line went out inside the message of an earlier line of its
    /// topic, which got the actual answer. Only after that message was
    /// accepted: when it fails, the merged lines' receivers are dropped.
    Merged,
}

pub type Delivery = Result<Outcome, ApiError>;

/// What actually talks to Telegram. `BotApi` in production, a fake in tests.
pub trait Transport: Send + Sync + 'static {
    fn execute(&self, op: &Op) -> impl Future<Output = Delivery> + Send;
}

impl Transport for BotApi {
    async fn execute(&self, op: &Op) -> Delivery {
        match op {
            Op::Send {
                chat,
                thread_id,
                text,
                html,
                reply_markup,
                reply_to,
                notify,
                ..
            } => {
                let (text, parse_mode) = formatted(text, html.as_deref());
                self.send_message(
                    Place::new(*chat, *thread_id),
                    text,
                    reply_markup.as_ref(),
                    parse_mode,
                    *reply_to,
                    *notify,
                )
                .await
                .map(Outcome::Sent)
            }
            Op::SendDocument {
                chat,
                thread_id,
                document,
                notify,
            } => self
                .send_document(Place::new(*chat, *thread_id), document, *notify)
                .await
                .map(Outcome::Sent),
            Op::SendPhoto {
                chat,
                thread_id,
                document,
                notify,
            } => match self
                .send_photo(Place::new(*chat, *thread_id), document, *notify)
                .await
            {
                Err(error) if error.is_photo_refusal() => {
                    warn!("telegram did not take a picture as a photo; sending it as a document");
                    self.send_document(Place::new(*chat, *thread_id), document, *notify)
                        .await
                }
                sent => sent,
            }
            .map(Outcome::Sent),
            Op::SendAlbum {
                chat,
                thread_id,
                items,
                photos,
                notify,
            } => match self
                .send_media_group(Place::new(*chat, *thread_id), items, *photos, *notify)
                .await
            {
                // Telegram names no item of a refused group, and its text
                // need not mention the photos (TASK-059 review): any 400
                // sends the same files once more as documents.
                Err(ApiError::Telegram { code: 400, .. }) if *photos => {
                    warn!("telegram did not take a photo album; sending it as documents");
                    self.send_media_group(Place::new(*chat, *thread_id), items, false, *notify)
                        .await
                }
                sent => sent,
            }
            .map(Outcome::Sent),
            Op::Edit {
                chat,
                message_id,
                text,
                reply_markup,
                ..
            } => self
                .edit_message_text(*chat, *message_id, text, None, reply_markup.as_ref())
                .await
                .map(|()| Outcome::Done),
            Op::AnswerCallback { query_id, text } => self
                .answer_callback_query(query_id, text.as_deref())
                .await
                .map(|()| Outcome::Done),
            Op::Delete { chat, message_id } => self
                .delete_message(*chat, *message_id)
                .await
                .map(|()| Outcome::Done),
            Op::Unpin { chat, message_id } => self
                .unpin_chat_message(*chat, *message_id)
                .await
                .map(|()| Outcome::Done),
            Op::CreateTopic {
                chat,
                name,
                icon_custom_emoji_id,
            } => self
                .create_forum_topic(*chat, name, icon_custom_emoji_id.as_deref())
                .await
                .map(Outcome::Topic),
            Op::EditTopic {
                chat,
                thread_id,
                name,
                icon_custom_emoji_id,
            } => self
                .edit_forum_topic(
                    *chat,
                    *thread_id,
                    name.as_deref(),
                    icon_custom_emoji_id.as_deref(),
                )
                .await
                .map(|()| Outcome::Done),
            Op::Stream {
                chat,
                text,
                html,
                into: Some(message_id),
                ..
            } => {
                let (text, parse_mode) = formatted(text, html.as_deref());
                // An explicit empty keyboard: the ⏹ of a status message
                // turning into turn content must go.
                self.edit_message_text(
                    *chat,
                    *message_id,
                    text,
                    parse_mode,
                    Some(&super::permissions::no_keyboard()),
                )
                .await
                .map(|()| {
                    Outcome::Sent(Message {
                        message_id: *message_id,
                        ..Message::default()
                    })
                })
            }
            Op::Stream {
                chat,
                thread_id,
                text,
                html,
                notify,
                into: None,
                ..
            } => {
                let (text, parse_mode) = formatted(text, html.as_deref());
                self.send_message(
                    Place::topic(*chat, *thread_id),
                    text,
                    None,
                    parse_mode,
                    None,
                    *notify,
                )
                .await
                .map(Outcome::Sent)
            }
            Op::React {
                chat,
                message_id,
                emoji,
            } => self
                .set_message_reaction(*chat, *message_id, emoji)
                .await
                .map(|()| Outcome::Done),
        }
    }
}

/// The text to send and its `parse_mode`: the HTML when there is one.
fn formatted<'a>(text: &'a str, html: Option<&'a str>) -> (&'a str, Option<&'static str>) {
    match html {
        Some(html) => (html, Some("HTML")),
        None => (text, None),
    }
}

/// Telegram could not parse the HTML of a message:
/// `400 Bad Request: can't parse entities: ...`.
fn is_bad_markup(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Telegram { code: 400, description }
            if description.to_ascii_lowercase().contains("can't parse entities")
    )
}

fn html_or_escaped(text: &str, html: Option<&str>) -> String {
    html.map_or_else(|| transcript::escape_html(text), str::to_owned)
}

/// Drops the HTML of a message op; true when it had some.
fn drop_html(op: &mut Op) -> bool {
    match op {
        Op::Send { html, .. } | Op::Stream { html, .. } => html.take().is_some(),
        _ => false,
    }
}

/// Token bucket for requests into the group.
///
/// Requests in any 60 s window are at most `capacity + 60 s / refill_every`.
/// The default is the one for new messages: 20 per minute, the documented
/// group limit, and `min_gap` keeps ~1 message/s per chat.
#[derive(Debug, Clone, Copy)]
pub struct BucketConfig {
    pub capacity: u32,
    pub refill_every: Duration,
    pub min_gap: Duration,
}

impl Default for BucketConfig {
    fn default() -> Self {
        Self {
            capacity: 5,
            refill_every: Duration::from_secs(4),
            min_gap: Duration::from_secs(1),
        }
    }
}

/// Edits, reactions and topic mutations of the whole group: at most 5 + 15 =
/// 20 per minute. Telegram publishes no number for them; with sends at 20
/// per minute and these unbounded, the live hub got three 429s in three
/// minutes (TASK-054). This allowance equals the message one, so the group
/// sees at most 40 requests per minute. Status refreshes are coalesced per
/// message and go round-robin in what foreground edits leave, so with N busy
/// status messages each one shows its newest text about every N × 4 s, while
/// a topic call or a foreground edit waits one or two tokens (4-8 s).
pub const EDIT_BUCKET: BucketConfig = BucketConfig {
    capacity: 5,
    refill_every: Duration::from_secs(4),
    min_gap: Duration::ZERO,
};

/// Every request into the group together (TASK-068): new messages, edits,
/// reactions, deletes, unpins and topic calls, at most 5 + 15 = 20 in any
/// 60 s. Telegram documents 20 messages a minute per group and no number for
/// the rest; v0.1.14 sent up to 40 requests a minute (both buckets above)
/// and got a 429 about every minute, so the others count too. After a 429
/// the rate adapts (see the module docs).
pub const GROUP_BUCKET: BucketConfig = BucketConfig {
    capacity: 5,
    refill_every: Duration::from_secs(4),
    min_gap: Duration::ZERO,
};

/// Quiet time a tool-call line waits for the next line of its topic: calls
/// of one step (parallel reads, a quick search) finish well within it, so a
/// burst becomes one message, and a lone line still shows within 1.5 s.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);

/// Longest wait of a tool-call line: lines that never pause for 1.5 s still
/// show every 4 s, one message each time (15 per minute for such a topic,
/// below the group's 20).
pub const DEBOUNCE_MAX: Duration = Duration::from_secs(4);

/// How the scheduler paces the group.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// New messages: `sendMessage`, `sendDocument`, `sendPhoto`, stream lines.
    pub messages: BucketConfig,
    /// Edits, reactions and topic mutations; `None` leaves them unmetered.
    pub edits: Option<BucketConfig>,
    /// Every request into the group but a callback answer, on top of the
    /// two above (TASK-068); `None`: only those.
    pub group: Option<BucketConfig>,
    /// Quiet time a `merge` stream line waits for more lines of its topic;
    /// zero turns the debounce off.
    pub debounce: Duration,
    /// Longest such wait, from the moment the line was handed over.
    pub debounce_max: Duration,
}

impl Default for Limits {
    /// The hub's pacing.
    fn default() -> Self {
        Self {
            messages: BucketConfig::default(),
            edits: Some(EDIT_BUCKET),
            group: Some(GROUP_BUCKET),
            debounce: DEBOUNCE,
            debounce_max: DEBOUNCE_MAX,
        }
    }
}

impl From<BucketConfig> for Limits {
    /// Only the message bucket, as before TASK-054: no debounce, edits
    /// unmetered. For tests that need a fast or a special message bucket.
    fn from(messages: BucketConfig) -> Self {
        Self {
            messages,
            edits: None,
            group: None,
            debounce: Duration::ZERO,
            debounce_max: Duration::ZERO,
        }
    }
}

#[derive(Debug)]
struct Bucket {
    config: BucketConfig,
    tokens: f64,
    refilled_at: Instant,
    last_take: Option<Instant>,
    /// Share of the configured refill rate, below 1 after a 429 (TASK-068).
    rate: f64,
}

impl Bucket {
    fn new(config: BucketConfig, now: Instant) -> Self {
        Self {
            config,
            tokens: f64::from(config.capacity),
            refilled_at: now,
            last_take: None,
            rate: 1.0,
        }
    }

    fn refill(&mut self, now: Instant) {
        // A 429 pause still running refills nothing (`slow_down`).
        if now <= self.refilled_at {
            return;
        }
        let elapsed = now.duration_since(self.refilled_at).as_secs_f64();
        let gained = elapsed * self.rate / self.config.refill_every.as_secs_f64();
        self.tokens = (self.tokens + gained).min(f64::from(self.config.capacity));
        self.rate = (self.rate + elapsed / 60.0 * RECOVERY_PER_MINUTE).min(1.0);
        self.refilled_at = now;
    }

    /// Earliest instant a metered op may go out.
    fn ready_at(&mut self, now: Instant) -> Instant {
        self.refill(now);
        let token_at = if self.tokens >= 1.0 {
            now
        } else {
            now + self
                .config
                .refill_every
                .mul_f64((1.0 - self.tokens) / self.rate)
        };
        match self.last_take {
            Some(last) => token_at.max(last + self.config.min_gap),
            None => token_at,
        }
    }

    fn take(&mut self, now: Instant) {
        self.refill(now);
        self.tokens -= 1.0;
        self.last_take = Some(now);
    }

    /// Telegram answered 429 and the queue waits until `until` (TASK-068):
    /// the rate halves, down to [`SLOWEST_RATE`]; one token is there when
    /// the pause ends, for the refused request, and the next ones come at
    /// the new rate from then.
    fn slow_down(&mut self, until: Instant) {
        self.rate = (self.rate / 2.0).max(SLOWEST_RATE);
        self.tokens = 1.0;
        self.refilled_at = until;
    }
}

/// The text and buttons of a status message (TASK-062), new or edited, read
/// when the call goes out, not when it was queued: after a wait for the
/// group's budget it shows the status of that moment. The sender keeps a
/// clone to change it meanwhile and to learn what went out.
#[derive(Debug, Clone)]
pub struct LiveText(Arc<Mutex<LiveContent>>);

#[derive(Debug)]
struct LiveContent {
    current: (String, Value),
    sent: Option<(String, Value)>,
}

impl LiveText {
    pub fn new(text: String, keyboard: Value) -> Self {
        Self(Arc::new(Mutex::new(LiveContent {
            current: (text, keyboard),
            sent: None,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, LiveContent> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the message is to show when it goes.
    pub fn set(&self, text: String, keyboard: Value) {
        self.lock().current = (text, keyboard);
    }

    /// What the last try sent, or what it is to show when none went; what
    /// Telegram shows only once the call was accepted.
    pub fn shown(&self) -> (String, Value) {
        let content = self.lock();
        content
            .sent
            .clone()
            .unwrap_or_else(|| content.current.clone())
    }

    /// The same text: `other` is a clone of this one.
    pub fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Puts the newest text and buttons into `op`, a `Send` or an `Edit`.
    fn fill(&self, op: &mut Op) {
        let mut content = self.lock();
        if let Op::Send {
            text, reply_markup, ..
        }
        | Op::Edit {
            text, reply_markup, ..
        } = op
        {
            let (current, keyboard) = content.current.clone();
            text.clone_from(&current);
            *reply_markup = Some(keyboard.clone());
            content.sent = Some((current, keyboard));
        }
    }
}

struct Job {
    op: Op,
    /// When it was handed over; the debounce counts from here.
    queued_at: Instant,
    reply: oneshot::Sender<Delivery>,
    /// Stream lines sent inside this job's message.
    merged: Vec<oneshot::Sender<Delivery>>,
    /// Goes again as plain text after Telegram refused its HTML: never takes
    /// more lines in, so it cannot become HTML and be refused a second time.
    plain_retry: bool,
    /// A status message (TASK-062): its text is read when it goes, and a
    /// permission prompt of its topic answers a new one `Superseded` while it
    /// waits.
    status: Option<LiveText>,
}

/// Cloneable handle that enqueues outbound operations.
#[derive(Clone, Debug)]
pub struct Outbox {
    tx: mpsc::Sender<Job>,
}

impl Outbox {
    /// Enqueues `op`. The receiver resolves when Telegram answered; it errors
    /// only if the scheduler has stopped. Dropping it makes the op fire-and-forget.
    pub async fn submit(&self, op: Op) -> oneshot::Receiver<Delivery> {
        self.submit_job(op, None).await
    }

    /// Enqueues a new status message, a `Send`, or an `Edit` of one
    /// (TASK-062): it goes with the text `status` holds then. A new status
    /// message never holds a permission prompt (or question) of its topic
    /// back: when one comes while it waits, it is answered `Superseded` and
    /// its sender sends it again below the prompt.
    pub async fn submit_status(&self, op: Op, status: LiveText) -> oneshot::Receiver<Delivery> {
        self.submit_job(op, Some(status)).await
    }

    async fn submit_job(&self, op: Op, status: Option<LiveText>) -> oneshot::Receiver<Delivery> {
        let (reply, receiver) = oneshot::channel();
        // A send error drops the job and its reply sender, so the receiver
        // reports the stopped scheduler by itself.
        let _ = self
            .tx
            .send(Job {
                op,
                queued_at: Instant::now(),
                reply,
                merged: Vec::new(),
                plain_retry: false,
                status,
            })
            .await;
        receiver
    }
}

pub struct Scheduler<T> {
    transport: Arc<T>,
    rx: mpsc::Receiver<Job>,
    open: bool,
    bucket: Bucket,
    /// Edits, reactions and topic mutations; `None`: unmetered.
    edit_bucket: Option<Bucket>,
    /// Every request but a callback answer (TASK-068); `None`: unmetered.
    group: Option<Bucket>,
    debounce: Duration,
    debounce_max: Duration,
    paused_until: Option<Instant>,
    consecutive_unmetered: usize,
    /// Requests into the group since the last status refresh went.
    since_refresh: usize,
    /// When topic calls and foreground edits both wait, a topic call goes
    /// next; they take turns.
    topic_turn: bool,
    /// Topics whose stream broke: their lines wait for a `restart` line.
    broken: HashSet<Place>,
    edit: VecDeque<Job>,
    topic: VecDeque<Job>,
    message: VecDeque<Job>,
}

enum Pick {
    Now(Lane),
    At(Instant),
    Idle,
}

impl<T: Transport> Scheduler<T> {
    /// `limits`: [`Limits::default`] in the hub; a bare [`BucketConfig`]
    /// paces new messages only.
    pub fn new(transport: Arc<T>, limits: impl Into<Limits>) -> (Self, Outbox) {
        let limits = limits.into();
        let now = Instant::now();
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let scheduler = Self {
            transport,
            rx,
            open: true,
            bucket: Bucket::new(limits.messages, now),
            edit_bucket: limits.edits.map(|edits| Bucket::new(edits, now)),
            group: limits.group.map(|group| Bucket::new(group, now)),
            debounce: limits.debounce,
            debounce_max: limits.debounce_max,
            paused_until: None,
            consecutive_unmetered: 0,
            since_refresh: 0,
            topic_turn: true,
            broken: HashSet::new(),
            edit: VecDeque::new(),
            topic: VecDeque::new(),
            message: VecDeque::new(),
        };
        (scheduler, Outbox { tx })
    }

    /// Runs until every `Outbox` is dropped and the queue is empty.
    pub async fn run(mut self) {
        loop {
            while let Ok(job) = self.rx.try_recv() {
                self.enqueue(job);
            }
            match self.pick(Instant::now()) {
                Pick::Now(lane) => self.dispatch(lane).await,
                Pick::At(when) if self.open => {
                    tokio::select! {
                        job = self.rx.recv() => self.receive(job),
                        () = sleep_until(when) => {}
                    }
                }
                Pick::At(when) => sleep_until(when).await,
                Pick::Idle if self.open => {
                    let job = self.rx.recv().await;
                    self.receive(job);
                }
                Pick::Idle => return,
            }
        }
    }

    fn receive(&mut self, job: Option<Job>) {
        match job {
            Some(job) => self.enqueue(job),
            None => self.open = false,
        }
    }

    fn lane_mut(&mut self, lane: Lane) -> &mut VecDeque<Job> {
        match lane {
            Lane::Edit(_) => &mut self.edit,
            Lane::Topic => &mut self.topic,
            Lane::Message(_) => &mut self.message,
        }
    }

    /// The oldest queued permission prompt with no older message of its
    /// topic, or with a debounce the first stream line of its topic when that
    /// is a `merge` line: it goes first, with the lines that join it.
    fn next_permission(&self) -> Option<usize> {
        let mut busy_topics = HashSet::new();
        // The first stream line of each topic and whether it is `merge`.
        let mut first_lines = HashMap::new();
        for (index, job) in self.message.iter().enumerate() {
            let Some(place) = job.op.place() else {
                continue;
            };
            let permission = match &job.op {
                Op::Send { permission, .. } => *permission,
                Op::SendDocument { .. } | Op::SendPhoto { .. } | Op::SendAlbum { .. } => false,
                // A write into a message above neither waits for a prompt
                // nor holds it back.
                Op::Stream { into: Some(_), .. } => continue,
                // Stream lines yield to a prompt of their own topic, except
                // tool-call lines the debounce holds: those came first.
                Op::Stream { merge, .. } => {
                    first_lines.entry(place).or_insert((index, *merge));
                    continue;
                }
                _ => continue,
            };
            if permission && !busy_topics.contains(&place) {
                return match first_lines.get(&place) {
                    Some(&(line, true)) if !self.debounce.is_zero() => Some(line),
                    _ => Some(index),
                };
            }
            busy_topics.insert(place);
        }
        None
    }

    fn enqueue(&mut self, job: Job) {
        if let (Op::Stream { restart, .. }, Some(place)) = (&job.op, job.op.place()) {
            if *restart {
                self.broken.remove(&place);
            } else if self.broken.contains(&place) {
                // Dropped unsent: its receiver closes without an answer.
                return;
            }
        }
        if let Op::React { emoji, .. } = &job.op
            && let Some(queued) = self.edit.iter_mut().find(|queued| {
                matches!(queued.op, Op::React { .. }) && queued.op.message() == job.op.message()
            })
        {
            if let Op::React {
                emoji: queued_emoji,
                ..
            } = &mut queued.op
            {
                queued_emoji.clone_from(emoji);
            }
            let superseded = std::mem::replace(&mut queued.reply, job.reply);
            let _ = superseded.send(Ok(Outcome::Superseded));
            return;
        }
        if let Op::Edit {
            text,
            reply_markup,
            background,
            ..
        } = &job.op
        {
            let pending = self.edit.iter_mut().find(|queued| {
                matches!(queued.op, Op::Edit { .. }) && queued.op.message() == job.op.message()
            });
            if let Some(queued) = pending {
                if let Op::Edit {
                    text: queued_text,
                    reply_markup: queued_markup,
                    background: queued_background,
                    ..
                } = &mut queued.op
                {
                    queued_text.clone_from(text);
                    queued_markup.clone_from(reply_markup);
                    // Foreground when either one is: a ⏹ press must not
                    // wait behind the refreshes it replaced.
                    *queued_background &= *background;
                }
                // The newer one's text is read when it goes; one without
                // (an old status message emptied) goes as it is.
                queued.status = job.status;
                let superseded = std::mem::replace(&mut queued.reply, job.reply);
                let _ = superseded.send(Ok(Outcome::Superseded));
                return;
            }
        }
        if let Op::Stream {
            chat,
            into: Some(message_id),
            ..
        } = &job.op
        {
            // A status refresh or a ⏹ edit still queued for the message this
            // content goes into would overwrite it.
            let target = Some(MessageKey::new(*chat, *message_id));
            let mut index = 0;
            while index < self.edit.len() {
                if matches!(self.edit[index].op, Op::Edit { .. })
                    && self.edit[index].op.message() == target
                    && let Some(old) = self.edit.remove(index)
                {
                    let _ = old.reply.send(Ok(Outcome::Superseded));
                } else {
                    index += 1;
                }
            }
        }
        if let (
            Op::Send {
                permission: true, ..
            },
            Some(place),
        ) = (&job.op, job.op.place())
        {
            // A status message of the prompt's topic still queued would hold
            // it back (TASK-062): it goes again below the prompt. One put
            // back after a 429 is queued again before the prompt, which came
            // while it was out, is taken in.
            let mut index = 0;
            while index < self.message.len() {
                if self.message[index].status.is_some()
                    && self.message[index].op.place() == Some(place)
                    && let Some(old) = self.message.remove(index)
                {
                    let _ = old.reply.send(Ok(Outcome::Superseded));
                } else {
                    index += 1;
                }
            }
        }
        let lane = job.op.lane();
        self.lane_mut(lane).push_back(job);
    }

    fn pick(&mut self, now: Instant) -> Pick {
        if let Some(until) = self.paused_until {
            if now < until {
                return Pick::At(until);
            }
            self.paused_until = None;
        }
        // Every request waits for the group's token too.
        let group_ready = self.group.as_mut().map_or(now, |b| b.ready_at(now));
        let message_ready = self.bucket.ready_at(now).max(group_ready);
        let edit_ready = self
            .edit_bucket
            .as_mut()
            .map_or(now, |b| b.ready_at(now))
            .max(group_ready);
        let permission = self.next_permission();
        if let Some(index) = permission
            && self.ready_at(index, message_ready, edit_ready) <= now
        {
            return Pick::Now(Lane::Message(index));
        }
        let message = self.next_message(now, message_ready, edit_ready);
        // With one budget for the group (TASK-068) turn content written into
        // a message is no new message here: it waits its turn below.
        if let Some((at, index)) = message
            && at <= now
            && self.consecutive_unmetered >= MAX_CONSECUTIVE_UNMETERED
            && (self.group.is_none() || self.message[index].op.metered())
        {
            return Pick::Now(Lane::Message(index));
        }
        // Due stream messages written into an existing one compete for the
        // edit token: one that is not `merge` (the status message turning
        // into turn content, TASK-062) with the foreground edits, the others
        // (the turn message growing) with the background ones.
        let content = [true, false].map(|first| self.due_content(now, edit_ready, first));
        // A message token no new message wants: the turn content takes it,
        // and the edit token goes to a status refresh. Not with one budget
        // for the group: both would take its token.
        let spare = self.group.is_none()
            && message_ready <= now
            && message.is_none_or(|(at, index)| at > now || !self.message[index].op.metered());
        // With one budget for the group (TASK-068) a growing turn message
        // does not share the status refreshes' turn: it goes with the new
        // messages, below.
        let shared = [content[0], content[1].filter(|_| self.group.is_none())];
        if let Some(lane) = self.next_edit(edit_ready <= now, shared, spare) {
            // The group's token goes to a due new message or growing turn
            // message before a status refresh, but for one in five.
            if matches!(lane, Lane::Edit(index) if self.edit[index].op.background())
                && self.since_refresh < MAX_CONSECUTIVE_UNMETERED
                && let Some(index) = self.group_content(message, content[1], now)
            {
                return Pick::Now(Lane::Message(index));
            }
            return Pick::Now(lane);
        }
        if let Some(index) = self.group_content(message, content[1], now) {
            return Pick::Now(Lane::Message(index));
        }
        // Edits or topic mutations left wait for the edit bucket.
        let edits_at = (!self.edit.is_empty() || !self.topic.is_empty()).then_some(edit_ready);
        match message {
            Some((at, index)) if at <= now => Pick::Now(Lane::Message(index)),
            message => match message.map(|(at, _)| at).into_iter().chain(edits_at).min() {
                Some(at) => Pick::At(at),
                None => Pick::Idle,
            },
        }
    }

    /// With one budget for the group (TASK-068): the older of the due new
    /// message `message` ([`Self::next_message`]) and the due growing turn
    /// message `growing`; `None` without a group budget.
    fn group_content(
        &self,
        message: Option<(Instant, usize)>,
        growing: Option<usize>,
        now: Instant,
    ) -> Option<usize> {
        self.group.as_ref()?;
        message
            .filter(|&(at, index)| at <= now && self.message[index].op.metered())
            .map(|(_, index)| index)
            .into_iter()
            .chain(growing)
            .min_by_key(|&index| self.message[index].queued_at)
    }

    /// When the token the `Message` job at `index` takes is there: a new
    /// message's or, for one written into an existing message, an edit's or
    /// a spare message token (see [`Self::spills`]).
    fn ready_at(&self, index: usize, message_ready: Instant, edit_ready: Instant) -> Instant {
        if self.message[index].op.metered() {
            message_ready
        } else {
            edit_ready.min(message_ready)
        }
    }

    /// Turn content (a stream message written into an existing message)
    /// takes a message token when no edit token is there (TASK-062): turn
    /// lines used to be new messages, and with them in edits the message
    /// budget has room while the edit budget has none. The group still sees
    /// at most the two budgets together.
    fn spills(&mut self, op: &Op, now: Instant) -> bool {
        matches!(op, Op::Stream { into: Some(_), .. })
            && self
                .edit_bucket
                .as_mut()
                .is_some_and(|bucket| bucket.ready_at(now) > now)
    }

    /// The first due `Message` job, first of its queue ([`Op::queue`]), that
    /// is written into an existing message: a `first` one (not `merge`) or a
    /// growing one.
    fn due_content(&self, now: Instant, edit_ready: Instant, first: bool) -> Option<usize> {
        let mut queues = HashSet::new();
        (0..self.message.len()).find(|&index| {
            let job = &self.message[index];
            queues.insert(job.op.queue())
                && matches!(&job.op, Op::Stream { into: Some(_), merge, .. } if *merge != first)
                && self.due(index).max(edit_ready) <= now
        })
    }

    /// The unmetered job to send: the oldest callback answer (no token), and
    /// with a token a topic call or the oldest foreground edit - or else
    /// `content[0]`, a status message turning into turn content (TASK-062) -
    /// (in turns when both wait), else the oldest of the background edits
    /// and `content[1]`, a turn message growing; the background edit when a
    /// `spare` message token can take the growing turn message instead.
    fn next_edit(
        &self,
        bucket_ready: bool,
        content: [Option<usize>; 2],
        spare: bool,
    ) -> Option<Lane> {
        if let Some(index) = self.edit.iter().position(|job| !job.op.edit_metered()) {
            return Some(Lane::Edit(index));
        }
        if !bucket_ready {
            return None;
        }
        let foreground = self
            .edit
            .iter()
            .position(|job| !job.op.background())
            .map(Lane::Edit)
            .or(content[0].map(Lane::Message));
        match (self.topic.is_empty(), foreground) {
            (false, Some(lane)) if !self.topic_turn => Some(lane),
            (false, _) => Some(Lane::Topic),
            (true, Some(lane)) => Some(lane),
            // Turn content and status refreshes share what is left, oldest
            // first: neither starves the other.
            (true, None) if spare && !self.edit.is_empty() => Some(Lane::Edit(0)),
            (true, None) => self
                .edit
                .front()
                .map(|job| (job.queued_at, Lane::Edit(0)))
                .into_iter()
                .chain(
                    content[1].map(|index| (self.message[index].queued_at, Lane::Message(index))),
                )
                .min_by_key(|(queued_at, _)| *queued_at)
                .map(|(_, lane)| lane),
        }
    }

    /// The message to send next and when its debounce and its bucket allow
    /// it: the first one that has no older job of its queue ([`Op::queue`])
    /// queued and is due, else the one due soonest.
    fn next_message(
        &self,
        now: Instant,
        message_ready: Instant,
        edit_ready: Instant,
    ) -> Option<(Instant, usize)> {
        let mut queues = HashSet::new();
        let mut soonest: Option<(Instant, usize)> = None;
        // Turn content that is due goes only when no new message is: it
        // takes what the new messages leave of the message budget.
        let mut content = None;
        for (index, job) in self.message.iter().enumerate() {
            if !queues.insert(job.op.queue()) {
                continue;
            }
            let due = self
                .due(index)
                .max(self.ready_at(index, message_ready, edit_ready));
            if due <= now && job.op.metered() {
                return Some((due, index));
            }
            if due <= now {
                content = content.or(Some((due, index)));
            }
            if soonest.is_none_or(|(at, _)| due < at) {
                soonest = Some((due, index));
            }
        }
        content.or(soonest)
    }

    /// When the message at `index`, the first of its queue ([`Op::queue`]),
    /// may go: a `merge` line waits for a quiet `debounce` after the last
    /// line that would join it, at most `debounce_max` after it came, and not
    /// at all once a message of its queue that cannot join (a prompt
    /// included) is queued; anything else at once.
    fn due(&self, index: usize) -> Instant {
        let job = &self.message[index];
        let Op::Stream {
            merge: true,
            notify,
            into,
            ..
        } = &job.op
        else {
            return job.queued_at;
        };
        let queue = job.op.queue();
        if self.debounce.is_zero() || job.plain_retry {
            return job.queued_at;
        }
        let mut last = job.queued_at;
        for later in self.message.iter().skip(index + 1) {
            if later.op.queue() != queue {
                continue;
            }
            match &later.op {
                Op::Stream {
                    merge: true,
                    notify: later_notify,
                    into: later_into,
                    ..
                } if later_notify == notify && later_into == into => last = later.queued_at,
                // A prompt of the topic (it lets the lines before it go
                // first, at once), or anything else that cannot join: no
                // reason to wait, and nothing after it joins the message.
                _ => return job.queued_at,
            }
        }
        (last + self.debounce).min(job.queued_at + self.debounce_max)
    }

    async fn dispatch(&mut self, lane: Lane) {
        let index = match lane {
            Lane::Message(index) | Lane::Edit(index) => index,
            // A new topic does not wait behind the deletes of old status
            // messages (TASK-062); the other topic calls keep their order, so
            // a delete never waits behind a stream of topic edits.
            Lane::Topic => self
                .topic
                .iter()
                .position(|job| matches!(job.op, Op::CreateTopic { .. }))
                .unwrap_or(0),
        };
        let Some(mut job) = self.lane_mut(lane).remove(index) else {
            return;
        };
        if matches!(lane, Lane::Message(_)) {
            self.merge_lines(&mut job, Instant::now());
        }
        if let Some(status) = &job.status {
            status.fill(&mut job.op);
        }
        if !matches!(job.op, Op::AnswerCallback { .. })
            && let Some(group) = &mut self.group
        {
            group.take(Instant::now());
            self.since_refresh = if job.op.background() {
                0
            } else {
                self.since_refresh.saturating_add(1)
            };
        }
        if job.op.metered() || self.spills(&job.op, Instant::now()) {
            self.bucket.take(Instant::now());
            self.consecutive_unmetered = 0;
        } else {
            if job.op.edit_metered()
                && let Some(bucket) = &mut self.edit_bucket
            {
                bucket.take(Instant::now());
            }
            self.consecutive_unmetered = self.consecutive_unmetered.saturating_add(1);
            match lane {
                Lane::Topic => self.topic_turn = false,
                Lane::Edit(_) if job.op.edit_metered() && !job.op.background() => {
                    self.topic_turn = true;
                }
                Lane::Message(_) if matches!(job.op, Op::Stream { merge: false, .. }) => {
                    self.topic_turn = true;
                }
                _ => {}
            }
        }
        let result = self.transport.execute(&job.op).await;
        if matches!(&result, Err(error) if is_bad_markup(error)) && drop_html(&mut job.op) {
            // Once: the op has no HTML left to refuse.
            warn!("telegram could not parse a formatted message; sending it as plain text");
            job.plain_retry = true;
            self.lane_mut(lane).push_front(job);
            return;
        }
        match result {
            Err(ApiError::RetryAfter(wait)) => {
                let wait = wait.max(MIN_RETRY_AFTER);
                let until = Instant::now() + wait;
                self.paused_until = Some(until);
                let group_rate = self.group.as_mut().map(|group| {
                    group.slow_down(until);
                    format!("{:.0}%", group.rate * 100.0)
                });
                warn!(
                    ?wait,
                    group_rate = group_rate.as_deref().unwrap_or("-"),
                    "telegram flood control, outbound queue paused"
                );
                // A newer edit of the message came while this one was out:
                // it carries the newest text and must not be overwritten.
                if let Some(newer) = self.edit.iter_mut().find(|q| q.op.replaces(&job.op)) {
                    if let (
                        Op::Edit { background, .. },
                        Op::Edit {
                            background: old, ..
                        },
                    ) = (&mut newer.op, &job.op)
                    {
                        *background &= *old;
                    }
                    let _ = job.reply.send(Ok(Outcome::Superseded));
                    return;
                }
                // Safe for a prompt or a line taken from the middle: nothing
                // older of its topic was queued, so the head keeps every
                // topic's order.
                self.lane_mut(lane).push_front(job);
            }
            result => {
                let accepted = result.is_ok();
                if let (Op::Stream { .. }, Some(place), Err(error)) =
                    (&job.op, job.op.place(), &result)
                    && !matches!(error, ApiError::Telegram { code, .. } if (400..500).contains(code))
                {
                    self.break_stream(place);
                }
                let _ = job.reply.send(result);
                // A refused message carried its merged lines with it: their
                // receivers close unanswered, never `Merged`.
                if accepted {
                    for merged in job.merged {
                        let _ = merged.send(Ok(Outcome::Merged));
                    }
                }
            }
        }
    }

    /// Drops the queued lines of `place`'s stream up to its next
    /// `restart` line, and every later one until such a line comes.
    fn break_stream(&mut self, place: Place) {
        self.broken.insert(place);
        let mut broken = true;
        self.message.retain(|job| match &job.op {
            Op::Stream { restart, .. } if job.op.place() == Some(place) => {
                broken &= !*restart;
                !broken
            }
            _ => true,
        });
        if !broken {
            self.broken.remove(&place);
        }
    }

    /// Joins the stream lines of `job`'s topic queued right after it into
    /// its text: always with a debounce, else when more messages wait than
    /// the bucket has tokens.
    fn merge_lines(&mut self, job: &mut Job, now: Instant) {
        if job.plain_retry {
            return;
        }
        let queue = job.op.queue();
        let Op::Stream {
            text,
            html,
            merge: true,
            notify,
            into,
            ..
        } = &mut job.op
        else {
            return;
        };
        if into.is_some() {
            // Written into a message: a later line into the same message
            // carries this text and more.
            let mut index = 0;
            while index < self.message.len() {
                let queued = &self.message[index].op;
                if queued.queue() != queue {
                    index += 1;
                    continue;
                }
                let Op::Stream {
                    text: next,
                    html: next_html,
                    merge: true,
                    notify: next_notify,
                    into: next_into,
                    ..
                } = queued
                else {
                    break;
                };
                if next_notify != notify || next_into != into {
                    break;
                }
                text.clone_from(next);
                html.clone_from(next_html);
                let Some(next) = self.message.remove(index) else {
                    break;
                };
                job.merged.push(next.reply);
                job.merged.extend(next.merged);
            }
            return;
        }
        self.bucket.refill(now);
        if self.debounce.is_zero() && (self.message.len() + 1) as f64 <= self.bucket.tokens {
            return;
        }
        let mut index = 0;
        while index < self.message.len() {
            let queued = &self.message[index].op;
            if queued.queue() != queue {
                index += 1;
                continue;
            }
            let Op::Stream {
                text: next,
                html: next_html,
                merge: true,
                notify: next_notify,
                into: None,
                ..
            } = queued
            else {
                break;
            };
            if next_notify != notify {
                break;
            }
            // One formatted line makes the whole message HTML.
            let joined_html = (html.is_some() || next_html.is_some()).then(|| {
                format!(
                    "{}\n{}",
                    html_or_escaped(text, html.as_deref()),
                    html_or_escaped(next, next_html.as_deref())
                )
            });
            let too_long =
                |text: &str| transcript::telegram_len(text) > transcript::TELEGRAM_TEXT_LIMIT;
            if transcript::telegram_len(text) + 1 + transcript::telegram_len(next)
                > transcript::TELEGRAM_TEXT_LIMIT
                || joined_html.as_deref().is_some_and(too_long)
            {
                break;
            }
            text.push('\n');
            text.push_str(next);
            *html = joined_html;
            let Some(next) = self.message.remove(index) else {
                break;
            };
            job.merged.push(next.reply);
            job.merged.extend(next.merged);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Clone)]
    struct Call {
        at: Duration,
        op: Op,
    }

    /// Records calls with their (paused-clock) time; answers 429 for the
    /// first `flood` calls and `refuse_code` for a message containing `refuse`.
    struct Fake {
        start: Instant,
        calls: Mutex<Vec<Call>>,
        flood: Mutex<VecDeque<Duration>>,
        delay: Duration,
        refuse: Option<&'static str>,
        refuse_code: i64,
    }

    impl Fake {
        fn new(flood: &[u64]) -> Arc<Self> {
            Self::with_delay(flood, Duration::ZERO)
        }

        fn with_delay(flood: &[u64], delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                calls: Mutex::new(Vec::new()),
                flood: Mutex::new(flood.iter().copied().map(Duration::from_secs).collect()),
                delay,
                refuse: None,
                refuse_code: 502,
            })
        }

        fn refusing(text: &'static str) -> Arc<Self> {
            Self::refusing_with(text, 502)
        }

        fn refusing_with(text: &'static str, code: i64) -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                calls: Mutex::new(Vec::new()),
                flood: Mutex::new(VecDeque::new()),
                delay: Duration::ZERO,
                refuse: Some(text),
                refuse_code: code,
            })
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }
    }

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(Call {
                    at: Instant::now() - self.start,
                    op: op.clone(),
                });
            }
            tokio::time::sleep(self.delay).await;
            let flood = self.flood.lock().ok().and_then(|mut f| f.pop_front());
            if let Some(wait) = flood {
                return Err(ApiError::RetryAfter(wait));
            }
            if let (Some(refuse), Op::Stream { text, .. }) = (self.refuse, op)
                && text.contains(refuse)
            {
                return Err(ApiError::Telegram {
                    code: self.refuse_code,
                    description: "refused".to_owned(),
                });
            }
            Ok(match op {
                Op::CreateTopic { .. } => Outcome::Topic(ForumTopic::default()),
                Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. } => {
                    Outcome::Sent(Message::default())
                }
                _ => Outcome::Done,
            })
        }
    }

    fn send(thread: i64, text: &str) -> Op {
        Op::Send {
            chat: Chat::Group,
            thread_id: Some(thread),
            text: text.to_owned(),
            html: None,
            reply_markup: None,
            permission: false,
            reply_to: None,
            notify: false,
        }
    }

    fn edit(message_id: i64, text: &str) -> Op {
        Op::Edit {
            chat: Chat::Group,
            message_id,
            text: text.to_owned(),
            reply_markup: None,
            background: false,
        }
    }

    /// A periodic status refresh.
    fn refresh(message_id: i64, text: &str) -> Op {
        Op::Edit {
            chat: Chat::Group,
            message_id,
            text: text.to_owned(),
            reply_markup: None,
            background: true,
        }
    }

    fn text_of(op: &Op) -> &str {
        match op {
            Op::Send { text, .. } | Op::Edit { text, .. } => text,
            Op::CreateTopic { name, .. } => name,
            Op::SendDocument { document, .. } => &document.file_name,
            _ => "",
        }
    }

    /// Enqueues everything first, then runs the scheduler to completion with
    /// the message bucket only (no debounce, edits unmetered).
    async fn run(fake: &Arc<Fake>, ops: Vec<Op>) -> Vec<Delivery> {
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let mut receivers = Vec::new();
        for op in ops {
            receivers.push(outbox.submit(op).await);
        }
        drop(outbox);
        scheduler.run().await;
        let mut results = Vec::new();
        for receiver in receivers {
            if let Ok(result) = receiver.await {
                results.push(result);
            }
        }
        results
    }

    #[test]
    fn default_bucket_fits_twenty_per_minute() {
        let config = BucketConfig::default();
        let refills = Duration::from_secs(60).as_secs_f64() / config.refill_every.as_secs_f64();
        assert!(f64::from(config.capacity) + refills <= 20.0);
        assert!(config.min_gap >= Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn group_limit_and_topic_order_hold() {
        let fake = Fake::new(&[]);
        let ops = (0..60)
            .map(|i| send(i % 3, &format!("{}:{}", i % 3, i / 3)))
            .collect();
        let results = run(&fake, ops).await;
        assert_eq!(results.len(), 60);
        assert!(results.iter().all(|r| matches!(r, Ok(Outcome::Sent(_)))));

        let calls = fake.calls();
        assert_eq!(calls.len(), 60);
        for (i, call) in calls.iter().enumerate() {
            let in_window = calls[i..]
                .iter()
                .take_while(|later| later.at < call.at + Duration::from_secs(60))
                .count();
            assert!(
                in_window <= 20,
                "{in_window} sends in the minute after {:?}",
                call.at
            );
        }
        for pair in calls.windows(2) {
            assert!(pair[1].at - pair[0].at >= Duration::from_secs(1));
        }
        for thread in 0..3 {
            let seq: Vec<u32> = calls
                .iter()
                .filter_map(|c| text_of(&c.op).split_once(':'))
                .filter(|(t, _)| *t == thread.to_string())
                .filter_map(|(_, n)| n.parse().ok())
                .collect();
            assert_eq!(seq, (0..20).collect::<Vec<_>>(), "thread {thread}");
        }
        // Not slower than needed: 5 burst + 55 refills of 4 s.
        assert!(calls[59].at <= Duration::from_secs(4 * 55 + 5));
    }

    #[tokio::test(start_paused = true)]
    async fn permission_prompt_jumps_the_queue() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..10).map(|i| send(1, &format!("m{i}"))).collect();
        ops.push(Op::Send {
            chat: Chat::Group,
            thread_id: Some(2),
            text: "permission".to_owned(),
            html: None,
            reply_markup: None,
            permission: true,
            reply_to: None,
            notify: true,
        });
        run(&fake, ops).await;
        let calls = fake.calls();
        assert_eq!(text_of(&calls[0].op), "permission");
        assert_eq!(text_of(&calls[1].op), "m0");
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_edits_of_one_message_coalesce() {
        let fake = Fake::new(&[]);
        let results = run(
            &fake,
            vec![edit(7, "a"), edit(8, "x"), edit(7, "b"), edit(7, "c")],
        )
        .await;
        let calls = fake.calls();
        let sent: Vec<&str> = calls.iter().map(|c| text_of(&c.op)).collect();
        assert_eq!(sent, ["c", "x"]);
        assert!(matches!(results[0], Ok(Outcome::Superseded)));
        assert!(matches!(results[1], Ok(Outcome::Done)));
        assert!(matches!(results[2], Ok(Outcome::Superseded)));
        assert!(matches!(results[3], Ok(Outcome::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn sustained_edits_do_not_starve_a_ready_message() {
        let fake = Fake::with_delay(&[], Duration::from_millis(200));
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let _first_edit = outbox.submit(edit(1, "e0")).await;
        let message = outbox.submit(send(1, "message")).await;

        let producer_outbox = outbox.clone();
        let producer = tokio::spawn(async move {
            for id in 2..=601 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(producer_outbox.submit(edit(id, "flowing")).await);
            }
        });
        drop(outbox);
        let scheduler = tokio::spawn(scheduler.run());

        let delivery = tokio::time::timeout(Duration::from_secs(3), message)
            .await
            .expect("a ready message must be served within a few seconds")
            .expect("scheduler must still be running");
        assert!(matches!(delivery, Ok(Outcome::Sent(_))));
        assert!(
            producer.await.is_ok(),
            "edits should flow for the full 60 s"
        );

        let message_at = fake
            .calls()
            .into_iter()
            .find(|call| text_of(&call.op) == "message")
            .map(|call| call.at);
        assert!(message_at.is_some_and(|at| at <= Duration::from_secs(3)));
        scheduler.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn topic_mutations_and_edits_do_not_spend_message_tokens() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..6).map(|i| send(1, &format!("m{i}"))).collect();
        for i in 0..10 {
            ops.push(Op::CreateTopic {
                chat: Chat::Group,
                name: format!("t{i}"),
                icon_custom_emoji_id: None,
            });
            ops.push(Op::EditTopic {
                chat: Chat::Group,
                thread_id: i,
                name: None,
                icon_custom_emoji_id: Some("5".to_owned()),
            });
            ops.push(Op::Delete {
                chat: Chat::Group,
                message_id: i,
            });
            ops.push(edit(100 + i, "e"));
        }
        run(&fake, ops).await;
        let calls = fake.calls();
        let unmetered: Vec<&Call> = calls.iter().filter(|c| !c.op.metered()).collect();
        let sends: Vec<&Call> = calls.iter().filter(|c| c.op.metered()).collect();
        assert_eq!(unmetered.len(), 40);
        // Unmetered ops never wait for the bucket or the 1 s gap.
        assert!(unmetered.iter().all(|c| c.at == Duration::ZERO));
        // The full burst of 5 is still available to messages afterwards.
        let burst: Vec<Duration> = sends.iter().map(|c| c.at).take(5).collect();
        assert_eq!(burst, (0..5).map(Duration::from_secs).collect::<Vec<_>>());
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_pauses_everything_and_retries_once() {
        let fake = Fake::new(&[7]);
        let ops = vec![
            send(1, "first"),
            send(1, "second"),
            Op::CreateTopic {
                chat: Chat::Group,
                name: "topic".to_owned(),
                icon_custom_emoji_id: None,
            },
        ];
        let results = run(&fake, ops).await;
        assert!(results.iter().all(Result::is_ok));
        let calls = fake.calls();
        let order: Vec<(&str, Duration)> = calls.iter().map(|c| (text_of(&c.op), c.at)).collect();
        // The topic op goes first (unmetered lane before messages) and eats the 429.
        assert_eq!(
            order,
            [
                ("topic", Duration::ZERO),
                ("topic", Duration::from_secs(7)),
                ("first", Duration::from_secs(7)),
                ("second", Duration::from_secs(8)),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_429_is_one_attempt_per_retry_after() {
        let fake = Fake::new(&[3, 3, 3]);
        let results = run(&fake, vec![send(1, "only"), send(2, "next")]).await;
        assert!(results.iter().all(Result::is_ok));
        let times: Vec<Duration> = fake.calls().iter().map(|c| c.at).collect();
        assert_eq!(
            times,
            [0, 3, 6, 9, 10].map(Duration::from_secs).to_vec(),
            "exactly one attempt per retry_after, no storm"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_retry_after_still_pauses_for_one_second() {
        let fake = Fake::new(&[0]);
        let results = run(&fake, vec![edit(1, "edit")]).await;
        assert!(results.iter().all(Result::is_ok));
        let times: Vec<Duration> = fake.calls().iter().map(|call| call.at).collect();
        assert_eq!(times, [Duration::ZERO, Duration::from_secs(1)]);
    }

    fn permission(thread: i64, text: &str) -> Op {
        Op::Send {
            chat: Chat::Group,
            thread_id: Some(thread),
            text: text.to_owned(),
            html: None,
            reply_markup: None,
            permission: true,
            reply_to: None,
            notify: true,
        }
    }

    fn texts(fake: &Fake) -> Vec<String> {
        fake.calls()
            .iter()
            .map(|c| text_of(&c.op).to_owned())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn permission_never_overtakes_its_own_topic() {
        let fake = Fake::new(&[]);
        let document = Op::SendDocument {
            chat: Chat::Group,
            thread_id: Some(7),
            document: Document {
                file_name: "doc".to_owned(),
                bytes: Vec::new(),
                caption: None,
            },
            notify: false,
        };
        run(
            &fake,
            vec![send(7, "ordinary"), document, permission(7, "prompt")],
        )
        .await;
        assert_eq!(texts(&fake), ["ordinary", "doc", "prompt"]);
    }

    #[tokio::test(start_paused = true)]
    async fn permission_overtakes_other_topics_only() {
        let fake = Fake::new(&[]);
        let ops = vec![
            send(1, "m0"),
            send(2, "x"),
            permission(2, "p2"),
            send(1, "m1"),
            send(1, "m2"),
            permission(3, "p3"),
        ];
        run(&fake, ops).await;
        // p3 has nothing older in topic 3; p2 waits for x, then jumps m1 and m2.
        assert_eq!(texts(&fake), ["p3", "m0", "x", "p2", "m1", "m2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_chain_in_one_topic_keeps_enqueue_order() {
        let fake = Fake::new(&[]);
        let ops = vec![
            permission(5, "p1"),
            send(5, "o"),
            permission(5, "p2"),
            send(6, "z"),
        ];
        run(&fake, ops).await;
        assert_eq!(texts(&fake), ["p1", "o", "p2", "z"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retried_permission_keeps_topic_order() {
        let fake = Fake::new(&[3]);
        let ops = vec![send(1, "m0"), permission(2, "p"), send(2, "after")];
        let results = run(&fake, ops).await;
        assert!(results.iter().all(Result::is_ok));
        let order: Vec<(String, Duration)> = fake
            .calls()
            .iter()
            .map(|c| (text_of(&c.op).to_owned(), c.at))
            .collect();
        let expected = [("p", 0), ("p", 3), ("m0", 4), ("after", 5)]
            .map(|(text, at)| (text.to_owned(), Duration::from_secs(at)));
        assert_eq!(order, expected);
    }

    /// A new status message of `thread` (TASK-062), as the slots actor
    /// hands it over.
    fn status(thread: i64, text: &str) -> (Op, LiveText) {
        let keyboard = serde_json::json!({"inline_keyboard": []});
        let mut op = send(thread, text);
        if let Op::Send { reply_markup, .. } = &mut op {
            *reply_markup = Some(keyboard.clone());
        }
        (op, LiveText::new(text.to_owned(), keyboard))
    }

    /// TASK-062: a status message never holds a permission prompt of its
    /// topic back; the one of another topic keeps its place.
    #[tokio::test(start_paused = true)]
    async fn a_permission_prompt_supersedes_the_queued_status_message_of_its_topic() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let m0 = outbox.submit(send(1, "m0")).await;
        let (op, text) = status(2, "s2");
        let s2 = outbox.submit_status(op, text).await;
        let (op, text) = status(3, "s3");
        let s3 = outbox.submit_status(op, text).await;
        let m1 = outbox.submit(send(1, "m1")).await;
        let p2 = outbox.submit(permission(2, "p2")).await;
        drop(outbox);
        scheduler.run().await;
        assert!(matches!(s2.await, Ok(Ok(Outcome::Superseded))));
        for sent in [m0, s3, m1, p2] {
            assert!(matches!(sent.await, Ok(Ok(Outcome::Sent(_)))));
        }
        assert_eq!(texts(&fake), ["p2", "m0", "s3", "m1"]);
    }

    /// TASK-062: a status message goes with the text it holds when it goes,
    /// not the one it was queued with, and tells what went.
    #[tokio::test(start_paused = true)]
    async fn a_status_message_goes_with_the_text_of_when_it_goes() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let (op, text) = status(2, "❓ Ждёт разрешения");
        let sent = outbox.submit_status(op, text.clone()).await;
        let keyboard = serde_json::json!({"inline_keyboard": [[{"text": "⏹"}]]});
        text.set("💤 Ждёт вас".to_owned(), keyboard.clone());
        drop(outbox);
        scheduler.run().await;
        assert!(matches!(sent.await, Ok(Ok(Outcome::Sent(_)))));
        let calls = fake.calls();
        assert!(
            matches!(
                &calls[0].op,
                Op::Send { text, reply_markup: Some(markup), .. }
                    if text == "💤 Ждёт вас" && *markup == keyboard
            ),
            "{calls:?}"
        );
        text.set("later".to_owned(), serde_json::json!({}));
        assert_eq!(text.shown(), ("💤 Ждёт вас".to_owned(), keyboard));
    }

    /// TASK-062: a status message that got a 429 does not go again ahead of
    /// a prompt of its topic that came while it was out.
    #[tokio::test(start_paused = true)]
    async fn a_status_message_refused_by_flood_control_yields_to_a_prompt_that_came_meanwhile() {
        let fake = Fake::with_delay(&[1], Duration::from_secs(1));
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let running = tokio::spawn(scheduler.run());
        let (op, text) = status(2, "s2");
        let s2 = outbox.submit_status(op, text).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let p2 = outbox.submit(permission(2, "p2")).await;
        drop(outbox);
        running.await.unwrap();
        assert!(matches!(s2.await, Ok(Ok(Outcome::Superseded))));
        assert!(matches!(p2.await, Ok(Ok(Outcome::Sent(_)))));
        assert_eq!(texts(&fake), ["s2", "p2"]);
    }

    /// TASK-062 code review: a status refresh goes with the text it holds
    /// when it goes; one that replaces it in the queue brings its own text.
    #[tokio::test(start_paused = true)]
    async fn a_status_refresh_goes_with_the_text_of_when_it_goes() {
        let keyboard = serde_json::json!({"inline_keyboard": []});
        let live = |text: &str| LiveText::new(text.to_owned(), keyboard.clone());
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        // Message 7: its text changes while it waits.
        let first = live("❓ Ждёт разрешения");
        let r7 = outbox
            .submit_status(refresh(7, "❓ Ждёт разрешения"), first.clone())
            .await;
        first.set("💤 Ждёт вас".to_owned(), keyboard.clone());
        // Message 8: a newer status edit replaces the queued one. Message 9:
        // an edit without a live text (an old status message emptied) does.
        let old = live("❓ old");
        let _r8 = outbox
            .submit_status(refresh(8, "❓ old"), old.clone())
            .await;
        let newer = live("⏹ newer");
        let _r8b = outbox
            .submit_status(edit(8, "⏹ newer"), newer.clone())
            .await;
        old.set("❓ stale".to_owned(), keyboard.clone());
        newer.set("⚙️ newest".to_owned(), keyboard.clone());
        let r9 = outbox
            .submit_status(refresh(9, "❓ s9"), live("❓ s9"))
            .await;
        let _r9b = outbox.submit(edit(9, "emptied")).await;
        drop(outbox);
        scheduler.run().await;
        assert!(matches!(r7.await, Ok(Ok(Outcome::Done))));
        assert!(matches!(r9.await, Ok(Ok(Outcome::Superseded))));
        let calls = fake.calls();
        let mut sent: Vec<(i64, &str)> = calls
            .iter()
            .filter_map(|call| match &call.op {
                Op::Edit { message_id, .. } => Some((*message_id, text_of(&call.op))),
                _ => None,
            })
            .collect();
        sent.sort_unstable();
        assert_eq!(sent, [(7, "💤 Ждёт вас"), (8, "⚙️ newest"), (9, "emptied")]);
        assert_eq!(first.shown(), ("💤 Ждёт вас".to_owned(), keyboard));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_attempts_spend_tokens() {
        // Five 429s of 1 s, then 25 more sends: every attempt, failed or not,
        // counts against 20 per minute.
        let fake = Fake::new(&[1; 5]);
        let ops = (0..26).map(|i| send(i % 2, &format!("m{i}"))).collect();
        let results = run(&fake, ops).await;
        assert_eq!(results.len(), 26);
        let calls = fake.calls();
        assert_eq!(calls.len(), 31);
        for (i, call) in calls.iter().enumerate() {
            let in_window = calls[i..]
                .iter()
                .take_while(|later| later.at < call.at + Duration::from_secs(60))
                .count();
            assert!(
                in_window <= 20,
                "{in_window} attempts in the minute after {:?}",
                call.at
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stops_after_outbox_is_dropped_and_queue_drained() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let handle = tokio::spawn(scheduler.run());
        let receiver = outbox.submit(send(1, "x")).await;
        drop(outbox);
        assert!(matches!(receiver.await, Ok(Ok(Outcome::Sent(_)))));
        assert!(handle.await.is_ok());
    }

    fn line(thread: i64, text: &str) -> Op {
        Op::Stream {
            chat: Chat::Group,
            thread_id: thread,
            text: text.to_owned(),
            html: None,
            merge: true,
            restart: false,
            notify: false,
            into: None,
        }
    }

    fn sent_texts(fake: &Fake, thread: i64) -> Vec<String> {
        fake.calls()
            .iter()
            .filter_map(|call| match &call.op {
                Op::Stream {
                    thread_id, text, ..
                } if *thread_id == thread => Some(text.clone()),
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                } if *t == thread => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lines_go_one_per_message_while_the_budget_has_room() {
        let fake = Fake::new(&[]);
        let ops = vec![line(1, "a ✓"), line(1, "b ✓"), line(1, "c ✓")];
        let results = run(&fake, ops).await;
        assert!(results.iter().all(|r| matches!(r, Ok(Outcome::Sent(_)))));
        assert_eq!(sent_texts(&fake, 1), ["a ✓", "b ✓", "c ✓"]);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lines_held_back_by_the_limit_merge_in_order_without_loss() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..20).map(|i| line(1, &format!("t1-{i}"))).collect();
        ops.push(send(1, "answer"));
        ops.extend((20..30).map(|i| line(1, &format!("t1-{i}"))));
        ops.extend((0..5).map(|i| line(2, &format!("t2-{i}"))));
        let count = ops.len();
        let results = run(&fake, ops).await;
        assert_eq!(results.len(), count, "every line got an answer");
        assert!(
            results
                .iter()
                .all(|r| matches!(r, Ok(Outcome::Sent(_) | Outcome::Merged)))
        );
        let topic: Vec<String> = sent_texts(&fake, 1);
        assert!(topic.len() < 21, "lines were merged: {topic:?}");
        let lines: Vec<&str> = topic.iter().flat_map(|text| text.split('\n')).collect();
        let mut want: Vec<String> = (0..20).map(|i| format!("t1-{i}")).collect();
        want.push("answer".to_owned());
        want.extend((20..30).map(|i| format!("t1-{i}")));
        assert_eq!(lines, want, "same lines, same order");
        // Nothing merges across the ordinary message of the topic.
        assert!(topic.contains(&"answer".to_owned()), "{topic:?}");
        let other: Vec<&str> = sent_texts(&fake, 2)
            .iter()
            .flat_map(|text| text.split('\n').map(str::to_owned).collect::<Vec<_>>())
            .map(|line| {
                if line.starts_with("t2-") {
                    "ok"
                } else {
                    "wrong"
                }
            })
            .collect();
        assert_eq!(other, ["ok"; 5]);
    }

    #[tokio::test(start_paused = true)]
    async fn loud_and_quiet_lines_never_share_a_message() {
        let fake = Fake::new(&[]);
        let loud = |text: &str| {
            let mut op = line(1, text);
            if let Op::Stream { notify, .. } = &mut op {
                *notify = true;
            }
            op
        };
        let mut ops: Vec<Op> = (0..10).map(|i| line(1, &format!("q{i}"))).collect();
        ops.extend((0..3).map(|i| loud(&format!("l{i}"))));
        ops.extend((10..20).map(|i| line(1, &format!("q{i}"))));
        run(&fake, ops).await;
        let messages: Vec<(bool, String)> = fake
            .calls()
            .into_iter()
            .filter_map(|call| match call.op {
                Op::Stream { notify, text, .. } => Some((notify, text)),
                _ => None,
            })
            .collect();
        assert!(messages.len() < 23, "lines were merged: {messages:?}");
        for (notify, text) in &messages {
            let prefix = if *notify { 'l' } else { 'q' };
            assert!(
                text.split('\n').all(|line| line.starts_with(prefix)),
                "{notify} {text:?}"
            );
        }
        let lines: Vec<String> = messages
            .iter()
            .flat_map(|(_, text)| text.split('\n').map(str::to_owned))
            .collect();
        let want: Vec<String> = (0..10)
            .map(|i| format!("q{i}"))
            .chain((0..3).map(|i| format!("l{i}")))
            .chain((10..20).map(|i| format!("q{i}")))
            .collect();
        assert_eq!(lines, want, "same lines, same order");
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_merged_message_answers_none_of_its_lines_as_merged() {
        let fake = Fake::refusing("t-7\n");
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let mut receivers = Vec::new();
        for i in 0..20 {
            receivers.push(outbox.submit(line(1, &format!("t-{i}"))).await);
        }
        drop(outbox);
        scheduler.run().await;
        let mut answers = Vec::new();
        for receiver in receivers {
            answers.push(receiver.await.ok());
        }
        let refused = fake
            .calls()
            .into_iter()
            .find_map(|call| match call.op {
                Op::Stream { text, .. } if text.contains("t-7\n") => Some(text),
                _ => None,
            })
            .expect("t-7 went out merged with the next line");
        let first_refused = (0..20)
            .find(|i| refused.split('\n').any(|line| line == format!("t-{i}")))
            .expect("refused lines");
        for (i, answer) in answers.iter().enumerate() {
            // The refused message and every line after it (its stream broke)
            // are answered unsent, never `Merged`.
            match answer {
                Some(Ok(Outcome::Sent(_) | Outcome::Merged)) => {
                    assert!(i < first_refused, "t-{i}");
                }
                Some(Err(_)) | None => assert!(i >= first_refused, "t-{i}: {answer:?}"),
                other => panic!("t-{i}: {other:?}"),
            }
        }
        let last = fake.calls().last().map(|call| call.op.clone());
        assert!(
            matches!(&last, Some(Op::Stream { text, .. }) if *text == refused),
            "nothing was sent after the refused message"
        );
    }

    fn stream_op(thread: i64, text: &str, restart: bool) -> Op {
        Op::Stream {
            chat: Chat::Group,
            thread_id: thread,
            text: text.to_owned(),
            html: None,
            merge: false,
            restart,
            notify: false,
            into: None,
        }
    }

    fn stream_calls(fake: &Fake, thread: i64) -> Vec<String> {
        fake.calls()
            .into_iter()
            .filter_map(|call| match call.op {
                Op::Stream {
                    thread_id, text, ..
                } if thread_id == thread => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn after_a_refused_line_its_topic_sends_nothing_until_a_restart_line() {
        let fake = Fake::refusing("s-1");
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let handle = tokio::spawn(scheduler.run());
        let mut early = Vec::new();
        for op in [
            stream_op(1, "s-0", true),
            stream_op(1, "s-1", false),
            stream_op(1, "s-2", false),
            stream_op(2, "other", false),
            stream_op(1, "s-3", false),
        ] {
            early.push(outbox.submit(op).await);
        }
        let mut answers = Vec::new();
        for receiver in early {
            answers.push(receiver.await.ok());
        }
        assert!(matches!(answers[0], Some(Ok(Outcome::Sent(_)))));
        assert!(matches!(answers[1], Some(Err(_))), "s-1 refused");
        assert!(answers[2].is_none(), "s-2 dropped unsent");
        assert!(
            matches!(answers[3], Some(Ok(Outcome::Sent(_)))),
            "other topics go on"
        );
        assert!(answers[4].is_none(), "s-3 dropped unsent");
        // Handed over after the refusal was answered, before the stream
        // starts again: not sent either.
        let late = outbox.submit(stream_op(1, "s-4", false)).await;
        assert!(late.await.is_err(), "s-4 dropped unsent");
        let again = outbox.submit(stream_op(1, "again", true)).await;
        let next = outbox.submit(stream_op(1, "next", false)).await;
        assert!(matches!(again.await, Ok(Ok(Outcome::Sent(_)))));
        assert!(matches!(next.await, Ok(Ok(Outcome::Sent(_)))));
        drop(outbox);
        assert!(handle.await.is_ok());
        assert_eq!(stream_calls(&fake, 1), ["s-0", "s-1", "again", "next"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_telegram_rejects_with_a_4xx_does_not_break_its_stream() {
        let fake = Fake::refusing_with("s-1", 400);
        let ops = vec![
            stream_op(1, "s-0", true),
            stream_op(1, "s-1", false),
            stream_op(1, "s-2", false),
        ];
        let results = run(&fake, ops).await;
        assert_eq!(results.len(), 3, "every line answered");
        assert!(matches!(results[2], Ok(Outcome::Sent(_))));
        assert_eq!(stream_calls(&fake, 1), ["s-0", "s-1", "s-2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_merged_message_stays_within_the_telegram_limit() {
        let fake = Fake::new(&[]);
        let long = "x".repeat(1500);
        let ops: Vec<Op> = (0..12).map(|_| line(1, &long)).collect();
        run(&fake, ops).await;
        let topic = sent_texts(&fake, 1);
        assert!(
            topic
                .iter()
                .all(|text| transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT)
        );
        assert_eq!(
            topic
                .iter()
                .map(|text| text.split('\n').count())
                .sum::<usize>(),
            12
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_prompt_overtakes_the_stream_lines_of_its_topic() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..8)
            .map(|i| Op::Stream {
                chat: Chat::Group,
                thread_id: 1,
                text: format!("s{i}"),
                html: None,
                merge: false,
                restart: false,
                notify: false,
                into: None,
            })
            .collect();
        ops.push(permission(1, "prompt"));
        run(&fake, ops).await;
        assert_eq!(texts(&fake)[0], "prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn reactions_are_unmetered_and_the_newest_one_per_message_wins() {
        let fake = Fake::new(&[]);
        let react = |id: i64, emoji: &str| Op::React {
            chat: Chat::Group,
            message_id: id,
            emoji: emoji.to_owned(),
        };
        let mut ops: Vec<Op> = (0..6).map(|i| send(1, &format!("m{i}"))).collect();
        ops.extend([react(5, "👀"), react(6, "👀"), react(5, "✍")]);
        let results = run(&fake, ops).await;
        assert!(matches!(results[6], Ok(Outcome::Superseded)));
        let reacts: Vec<(i64, String, Duration)> = fake
            .calls()
            .iter()
            .filter_map(|call| match &call.op {
                Op::React {
                    chat: Chat::Group,
                    message_id,
                    emoji,
                } => Some((*message_id, emoji.clone(), call.at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reacts,
            [
                (5, "✍".to_owned(), Duration::ZERO),
                (6, "👀".to_owned(), Duration::ZERO)
            ]
        );
    }

    /// Refuses every message with HTML as Telegram does when it cannot parse
    /// it, and with `plain_too` the plain retry as well.
    #[derive(Default)]
    struct BadMarkup {
        calls: Mutex<Vec<Op>>,
        plain_too: bool,
    }

    impl Transport for BadMarkup {
        async fn execute(&self, op: &Op) -> Delivery {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(op.clone());
            }
            let html = matches!(
                op,
                Op::Send { html: Some(_), .. } | Op::Stream { html: Some(_), .. }
            );
            if html || self.plain_too {
                return Err(ApiError::Telegram {
                    code: 400,
                    description: "Bad Request: can't parse entities: Unsupported start tag \"x\" at byte offset 0".to_owned(),
                });
            }
            Ok(Outcome::Sent(Message::default()))
        }
    }

    fn formatted_send(text: &str, html: &str) -> Op {
        Op::Send {
            chat: Chat::Group,
            thread_id: Some(1),
            text: text.to_owned(),
            html: Some(html.to_owned()),
            reply_markup: None,
            permission: false,
            reply_to: None,
            notify: false,
        }
    }

    fn sent(fake: &BadMarkup) -> Vec<(String, Option<String>)> {
        fake.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send { text, html, .. } | Op::Stream { text, html, .. } => Some((text, html)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn html_telegram_cannot_parse_goes_again_once_as_its_plain_text() {
        let fake = Arc::new(BadMarkup::default());
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let answer = outbox.submit(formatted_send("**a**", "<b>a</b>")).await;
        let line = outbox
            .submit(Op::Stream {
                chat: Chat::Group,
                thread_id: 1,
                text: "_b_".to_owned(),
                html: Some("<i>b</i>".to_owned()),
                merge: false,
                restart: true,
                notify: false,
                into: None,
            })
            .await;
        let next = outbox.submit(send(1, "after")).await;
        drop(outbox);
        scheduler.run().await;
        for receiver in [answer, line, next] {
            assert!(matches!(receiver.await, Ok(Ok(Outcome::Sent(_)))));
        }
        let own = |text: &str, html: Option<&str>| (text.to_owned(), html.map(str::to_owned));
        assert_eq!(
            sent(&fake),
            [
                own("**a**", Some("<b>a</b>")),
                own("**a**", None),
                own("_b_", Some("<i>b</i>")),
                own("_b_", None),
                own("after", None),
            ],
            "each refused message goes again at once, before the next one"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_plain_message_telegram_cannot_parse_is_not_sent_again() {
        let fake = Arc::new(BadMarkup {
            plain_too: true,
            ..BadMarkup::default()
        });
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let formatted = outbox.submit(formatted_send("**a**", "<b>a</b>")).await;
        let plain = outbox.submit(send(1, "plain")).await;
        drop(outbox);
        scheduler.run().await;
        for receiver in [formatted, plain] {
            assert!(matches!(
                receiver.await,
                Ok(Err(ApiError::Telegram { code: 400, .. }))
            ));
        }
        assert_eq!(sent(&fake).len(), 3, "one retry for the HTML one only");
    }

    #[tokio::test(start_paused = true)]
    async fn a_formatted_line_merged_with_plain_lines_makes_one_html_message() {
        let fake = Fake::new(&[]);
        let formatted = Op::Stream {
            chat: Chat::Group,
            thread_id: 1,
            text: "\u{1F4AD} **x**".to_owned(),
            html: Some("\u{1F4AD} <b>x</b>".to_owned()),
            merge: true,
            restart: false,
            notify: false,
            into: None,
        };
        let mut ops: Vec<Op> = (0..8).map(|i| line(1, &format!("• a<{i}> ✓"))).collect();
        ops.insert(6, formatted);
        run(&fake, ops).await;
        let merged = fake
            .calls()
            .into_iter()
            .find_map(|call| match call.op {
                Op::Stream {
                    text,
                    html: Some(html),
                    ..
                } => Some((text, html)),
                _ => None,
            })
            .expect("the formatted line went out");
        assert!(merged.0.contains("• a<5> ✓\n\u{1F4AD} **x**"), "{merged:?}");
        assert!(
            merged.1.contains("• a&lt;5&gt; ✓\n\u{1F4AD} <b>x</b>"),
            "plain lines are escaped: {merged:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_plain_retry_never_merges_formatted_lines_back_into_html() {
        // Five formatted lines and five tokens: the first goes alone, and its
        // plain retry finds fewer tokens than waiting lines, so it would merge.
        let fake = Arc::new(BadMarkup::default());
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let mut receivers = Vec::new();
        for i in 0..5 {
            let op = Op::Stream {
                chat: Chat::Group,
                thread_id: 1,
                text: format!("**l{i}**"),
                html: Some(format!("<b>l{i}</b>")),
                merge: true,
                restart: false,
                notify: false,
                into: None,
            };
            receivers.push(outbox.submit(op).await);
        }
        drop(outbox);
        scheduler.run().await;
        for receiver in receivers {
            assert!(matches!(
                receiver.await,
                Ok(Ok(Outcome::Sent(_) | Outcome::Merged))
            ));
        }
        let calls = sent(&fake);
        assert!(calls.len().is_multiple_of(2), "{calls:?}");
        for pair in calls.chunks(2) {
            assert!(pair[0].1.is_some(), "an HTML attempt first: {calls:?}");
            assert_eq!(
                pair[1],
                (pair[0].0.clone(), None),
                "then its own plain text"
            );
        }
        let lines: Vec<String> = calls
            .iter()
            .filter(|(_, html)| html.is_none())
            .flat_map(|(text, _)| text.split('\n').map(str::to_owned).collect::<Vec<_>>())
            .collect();
        let want: Vec<String> = (0..5).map(|i| format!("**l{i}**")).collect();
        assert_eq!(lines, want, "every line once, in order");
    }

    /// Submits each op at its offset from the start with the hub's pacing,
    /// runs until everything is answered, and returns the answers in order.
    async fn run_timed(fake: &Arc<Fake>, ops: Vec<(u64, Op)>) -> Vec<Option<Delivery>> {
        run_limited(fake, ops, Limits::default()).await
    }

    /// [`run_timed`] with the hub's two class budgets but not the group's
    /// one (TASK-068): for the rules between the classes.
    async fn run_classes(fake: &Arc<Fake>, ops: Vec<(u64, Op)>) -> Vec<Option<Delivery>> {
        let limits = Limits {
            group: None,
            ..Limits::default()
        };
        run_limited(fake, ops, limits).await
    }

    async fn run_limited(
        fake: &Arc<Fake>,
        ops: Vec<(u64, Op)>,
        limits: Limits,
    ) -> Vec<Option<Delivery>> {
        let (scheduler, outbox) = Scheduler::new(fake.clone(), limits);
        let handle = tokio::spawn(scheduler.run());
        let start = Instant::now();
        let mut receivers = Vec::new();
        for (at_ms, op) in ops {
            sleep_until(start + Duration::from_millis(at_ms)).await;
            receivers.push(outbox.submit(op).await);
        }
        drop(outbox);
        let mut answers = Vec::new();
        for receiver in receivers {
            answers.push(receiver.await.ok());
        }
        assert!(handle.await.is_ok());
        answers
    }

    fn stream_times(fake: &Fake) -> Vec<(String, Duration)> {
        fake.calls()
            .into_iter()
            .filter_map(|call| match call.op {
                Op::Stream { text, .. } => Some((text, call.at)),
                Op::Send { text, .. } => Some((text, call.at)),
                _ => None,
            })
            .collect()
    }

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn default_edit_budget_fits_twenty_per_minute() {
        let limits = Limits::default();
        let edits = limits.edits.expect("the hub meters edits");
        let refills = Duration::from_secs(60).as_secs_f64() / edits.refill_every.as_secs_f64();
        assert!(f64::from(edits.capacity) + refills <= 20.0);
        assert!(limits.debounce <= limits.debounce_max);
    }

    #[test]
    fn default_group_budget_fits_twenty_per_minute() {
        let group = Limits::default().group.expect("the hub meters the group");
        let refills = Duration::from_secs(60).as_secs_f64() / group.refill_every.as_secs_f64();
        assert!(f64::from(group.capacity) + refills <= 20.0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_lines_goes_as_one_message_with_budget_to_spare() {
        let fake = Fake::new(&[]);
        let answers = run_timed(
            &fake,
            vec![
                (0, line(1, "a ✓")),
                (500, line(1, "b ✓")),
                (1000, line(1, "c ✓")),
            ],
        )
        .await;
        assert_eq!(
            stream_times(&fake),
            [("a ✓\nb ✓\nc ✓".to_owned(), ms(2500))],
            "one message, 1.5 s after the last line"
        );
        assert!(matches!(answers[0], Some(Ok(Outcome::Sent(_)))));
        assert!(matches!(answers[1], Some(Ok(Outcome::Merged))));
        assert!(matches!(answers[2], Some(Ok(Outcome::Merged))));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lone_line_goes_after_the_quiet_window() {
        let fake = Fake::new(&[]);
        run_timed(&fake, vec![(0, line(1, "a ✓"))]).await;
        assert_eq!(stream_times(&fake), [("a ✓".to_owned(), DEBOUNCE)]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_steady_trickle_of_lines_still_shows_every_few_seconds() {
        let fake = Fake::new(&[]);
        let ops = (0..12)
            .map(|i| (i * 1000 + 100, line(1, &format!("l{i}"))))
            .collect();
        run_timed(&fake, ops).await;
        let sent = stream_times(&fake);
        assert_eq!(sent[0].1, ms(100) + DEBOUNCE_MAX, "{sent:?}");
        assert!(sent.len() < 12, "lines were merged: {sent:?}");
        let lines: Vec<String> = sent
            .iter()
            .flat_map(|(text, _)| text.split('\n').map(str::to_owned).collect::<Vec<_>>())
            .collect();
        let want: Vec<String> = (0..12).map(|i| format!("l{i}")).collect();
        assert_eq!(lines, want, "every line once, in order");
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_prompt_lets_the_held_lines_of_its_topic_go_first_at_once() {
        let fake = Fake::new(&[]);
        run_timed(
            &fake,
            vec![
                (0, line(1, "a ✓")),
                (50, line(1, "b ✓")),
                (100, permission(1, "prompt")),
                (200, line(1, "c ✓")),
            ],
        )
        .await;
        // The lines before the prompt go at once, as one message, then the
        // prompt after the 1 s gap; the line after it waits its debounce.
        assert_eq!(
            stream_times(&fake),
            [
                ("a ✓\nb ✓".to_owned(), ms(100)),
                ("prompt".to_owned(), ms(1100)),
                ("c ✓".to_owned(), ms(2100))
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_of_another_topic_still_goes_before_waiting_lines() {
        let fake = Fake::new(&[]);
        run_timed(
            &fake,
            vec![(0, line(1, "a ✓")), (100, permission(2, "other"))],
        )
        .await;
        assert_eq!(
            stream_times(&fake),
            [("other".to_owned(), ms(100)), ("a ✓".to_owned(), DEBOUNCE)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lines_held_before_a_prompt_go_when_the_budget_allows_not_after_the_debounce() {
        let fake = Fake::new(&[]);
        run_timed(
            &fake,
            vec![
                (0, send(9, "x")),
                (100, line(1, "a ✓")),
                (200, permission(1, "prompt")),
            ],
        )
        .await;
        // The 1 s gap after "x" is the only wait: not 100 ms + 1.5 s.
        assert_eq!(
            stream_times(&fake),
            [
                ("x".to_owned(), ms(0)),
                ("a ✓".to_owned(), ms(1000)),
                ("prompt".to_owned(), ms(2000))
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiting_line_holds_back_only_its_own_topic() {
        let fake = Fake::new(&[]);
        run_timed(
            &fake,
            vec![
                (0, line(1, "a ✓")),
                (0, send(2, "other")),
                (0, send(1, "x")),
            ],
        )
        .await;
        // "x" cannot join the line, so the line does not wait for it.
        assert_eq!(
            stream_times(&fake),
            [
                ("a ✓".to_owned(), ms(0)),
                ("other".to_owned(), ms(1000)),
                ("x".to_owned(), ms(2000)),
            ]
        );
        let fake = Fake::new(&[]);
        run_timed(&fake, vec![(0, line(1, "a ✓")), (0, send(2, "other"))]).await;
        assert_eq!(
            stream_times(&fake),
            [("other".to_owned(), ms(0)), ("a ✓".to_owned(), DEBOUNCE)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_queued_after_the_lines_ends_their_wait() {
        let fake = Fake::new(&[]);
        run_timed(
            &fake,
            vec![
                (0, line(1, "a ✓")),
                (300, line(1, "b ✓")),
                (400, stream_op(1, "answer", false)),
            ],
        )
        .await;
        assert_eq!(
            stream_times(&fake),
            [
                ("a ✓\nb ✓".to_owned(), ms(400)),
                ("answer".to_owned(), ms(1400))
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_debounced_message_refused_with_429_goes_again_after_the_pause() {
        let fake = Fake::new(&[5]);
        let answers = run_timed(&fake, vec![(0, line(1, "a ✓")), (200, line(1, "b ✓"))]).await;
        assert_eq!(
            stream_times(&fake),
            [
                ("a ✓\nb ✓".to_owned(), ms(1700)),
                ("a ✓\nb ✓".to_owned(), ms(6700))
            ]
        );
        assert!(matches!(answers[0], Some(Ok(Outcome::Sent(_)))));
        assert!(matches!(answers[1], Some(Ok(Outcome::Merged))));
    }

    #[tokio::test(start_paused = true)]
    async fn edits_reactions_and_topic_mutations_stay_within_the_edit_budget() {
        let fake = Fake::new(&[]);
        let mut ops = Vec::new();
        for i in 0..40 {
            ops.push((0, edit(i, "status")));
            ops.push((
                0,
                Op::React {
                    chat: Chat::Group,
                    message_id: 1000 + i,
                    emoji: "👀".to_owned(),
                },
            ));
            ops.push((
                0,
                Op::EditTopic {
                    chat: Chat::Group,
                    thread_id: i,
                    name: None,
                    icon_custom_emoji_id: Some("5".to_owned()),
                },
            ));
            ops.push((0, send(i % 4, &format!("m{i}"))));
        }
        let answers = run_timed(&fake, ops).await;
        assert!(answers.iter().all(|a| matches!(a, Some(Ok(_)))));
        let calls = fake.calls();
        assert_eq!(calls.len(), 160);
        let edit_bucket = EDIT_BUCKET;
        let messages = BucketConfig::default();
        let allowed = |bucket: BucketConfig| {
            f64::from(bucket.capacity) + 60.0 / bucket.refill_every.as_secs_f64()
        };
        // TASK-068: everything together within the group's budget.
        let group = allowed(GROUP_BUCKET);
        for (i, call) in calls.iter().enumerate() {
            let window: Vec<&Call> = calls[i..]
                .iter()
                .take_while(|later| later.at < call.at + Duration::from_secs(60))
                .collect();
            let edits = window.iter().filter(|c| c.op.edit_metered()).count();
            let sends = window.iter().filter(|c| c.op.metered()).count();
            assert!(
                edits as f64 <= allowed(edit_bucket),
                "{edits} edits after {:?}",
                call.at
            );
            assert!(
                sends as f64 <= allowed(messages),
                "{sends} sends after {:?}",
                call.at
            );
            assert!(
                window.len() as f64 <= group,
                "{} requests after {:?}",
                window.len(),
                call.at
            );
        }
        // The group's tokens go to the foreground edits and topic calls
        // first, and to a waiting message after every
        // `MAX_CONSECUTIVE_UNMETERED` of them: the fifth token, the burst's
        // at 0 s, then one every 5 x 4 s.
        let sends: Vec<Duration> = calls
            .iter()
            .filter(|c| c.op.metered())
            .map(|c| c.at)
            .take(5)
            .collect();
        let every = GROUP_BUCKET.refill_every * (MAX_CONSECUTIVE_UNMETERED as u32 + 1);
        assert_eq!(
            sends,
            (0..5u32).map(|k| every * k).collect::<Vec<_>>(),
            "{sends:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn callback_answers_do_not_wait_for_the_edit_budget() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<(u64, Op)> = (0..20).map(|i| (0, edit(i, "status"))).collect();
        ops.push((
            100,
            Op::AnswerCallback {
                query_id: "q".to_owned(),
                text: None,
            },
        ));
        run_timed(&fake, ops).await;
        let calls = fake.calls();
        let answered = calls
            .iter()
            .find(|c| matches!(c.op, Op::AnswerCallback { .. }))
            .map(|c| c.at);
        assert_eq!(answered, Some(ms(100)));
        let last_edit = calls.iter().rev().find(|c| c.op.edit_metered());
        assert!(last_edit.is_some_and(|c| c.at >= Duration::from_secs(60)));
    }

    /// Status messages `100..100 + count` refreshed forever the way
    /// `pump_status` does it: one refresh per message handed over at a time,
    /// the next one 5 s after the last one was handed over. Each refresh
    /// records `(message, submitted, answered)`.
    fn refresh_forever(outbox: &Outbox, count: i64) -> Arc<Mutex<Vec<(i64, Instant, Instant)>>> {
        let log = Arc::new(Mutex::new(Vec::new()));
        for message in 100..100 + count {
            let outbox = outbox.clone();
            let log = log.clone();
            tokio::spawn(async move {
                for tick in 0u64.. {
                    let submitted = Instant::now();
                    let answer = outbox.submit(refresh(message, &format!("{tick}"))).await;
                    if answer.await.is_err() {
                        return;
                    }
                    if let Ok(mut log) = log.lock() {
                        log.push((message, submitted, Instant::now()));
                    }
                    sleep_until(submitted + Duration::from_secs(5)).await;
                }
            });
        }
        log
    }

    /// Hands `op` over and waits for its answer: how long it took. A starved
    /// op fails after a minute instead of hanging the test.
    async fn waited(outbox: &Outbox, op: Op) -> Duration {
        let start = Instant::now();
        let answer = outbox.submit(op.clone()).await;
        let answer = tokio::time::timeout(Duration::from_secs(60), answer).await;
        assert!(
            matches!(answer, Ok(Ok(Ok(_)))),
            "{op:?} not answered within a minute"
        );
        Instant::now() - start
    }

    #[tokio::test(start_paused = true)]
    async fn topic_calls_and_foreground_edits_never_wait_behind_status_refreshes() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        // Ten busy slots ask for 120 refreshes a minute; the budget is 20.
        let refreshes = refresh_forever(&outbox, 10);
        tokio::time::sleep(Duration::from_secs(20)).await;
        let ops = [
            Op::CreateTopic {
                chat: Chat::Group,
                name: "new session".to_owned(),
                icon_custom_emoji_id: None,
            },
            edit(900, "✅ Разрешено"),
            Op::EditTopic {
                chat: Chat::Group,
                thread_id: 5,
                name: None,
                icon_custom_emoji_id: Some("5".to_owned()),
            },
            Op::React {
                chat: Chat::Group,
                message_id: 901,
                emoji: "👀".to_owned(),
            },
            Op::Unpin {
                chat: Chat::Group,
                message_id: 902,
            },
            Op::Delete {
                chat: Chat::Group,
                message_id: 903,
            },
            edit(904, "↳ Explore: итог"),
        ];
        // One at a time, at uneven moments, for three minutes.
        for round in 0..3u64 {
            for (index, op) in ops.iter().enumerate() {
                tokio::time::sleep(Duration::from_millis(3100 + 700 * index as u64)).await;
                let wait = waited(&outbox, op.clone()).await;
                assert!(
                    wait <= EDIT_BUCKET.refill_every,
                    "round {round}: {op:?} waited {wait:?}"
                );
            }
        }
        let calls = fake.calls();
        for (i, call) in calls.iter().enumerate() {
            let edits = calls[i..]
                .iter()
                .take_while(|later| later.at < call.at + Duration::from_secs(60))
                .filter(|c| c.op.edit_metered())
                .count();
            assert!(
                edits <= 20,
                "{edits} edits in the minute after {:?}",
                call.at
            );
        }
        assert!(
            refreshes.lock().map(|log| log.len()).unwrap_or(0) > 10,
            "refreshes went on meanwhile"
        );
        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn topic_calls_and_foreground_edits_take_turns_ahead_of_refreshes() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let _refreshes = refresh_forever(&outbox, 10);
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut receivers = Vec::new();
        for i in 0..3 {
            receivers.push(
                outbox
                    .submit(Op::Unpin {
                        chat: Chat::Group,
                        message_id: 900 + i,
                    })
                    .await,
            );
            receivers.push(outbox.submit(edit(950 + i, "decision")).await);
        }
        for receiver in receivers {
            let answer = tokio::time::timeout(Duration::from_secs(60), receiver).await;
            assert!(matches!(answer, Ok(Ok(Ok(_)))));
        }
        let order: Vec<char> = fake
            .calls()
            .iter()
            .filter_map(|call| match &call.op {
                Op::Unpin {
                    chat: Chat::Group,
                    message_id,
                } if *message_id >= 900 => Some('T'),
                Op::Edit {
                    message_id,
                    background: false,
                    ..
                } if *message_id >= 950 => Some('F'),
                Op::Edit {
                    background: true, ..
                } => Some('b'),
                _ => None,
            })
            .skip_while(|kind| *kind == 'b')
            .take(6)
            .collect();
        assert!(
            order == ['T', 'F', 'T', 'F', 'T', 'F'] || order == ['F', 'T', 'F', 'T', 'F', 'T'],
            "{order:?}"
        );
        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn every_status_refresh_goes_within_a_bound_and_the_slots_share_the_rest() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let refreshes = refresh_forever(&outbox, 10);
        // A foreground edit every 10 s takes 6 of the 15 tokens a minute;
        // ten refreshing slots share the other 9.
        let start = Instant::now();
        for id in 0..30u32 {
            sleep_until(start + Duration::from_secs(10 * u64::from(id))).await;
            drop(outbox.submit(edit(900 + i64::from(id), "x")).await);
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
        let log = refreshes.lock().map(|log| log.clone()).unwrap_or_default();
        let mut counts = HashMap::new();
        for (message, submitted, answered) in &log {
            *counts.entry(*message).or_insert(0usize) += 1;
            let wait = *answered - *submitted;
            assert!(
                wait <= Duration::from_secs(90),
                "message {message} waited {wait:?}"
            );
        }
        assert_eq!(counts.len(), 10, "every slot refreshed: {counts:?}");
        let (least, most) = (
            counts.values().min().copied().unwrap_or(0),
            counts.values().max().copied().unwrap_or(0),
        );
        assert!(least >= 3, "{counts:?}");
        assert!(most - least <= 1, "round-robin: {counts:?}");
        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_foreground_edit_takes_over_a_queued_refresh_of_its_message() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<(u64, Op)> = (0..5).map(|i| (0, refresh(10 + i, "drain"))).collect();
        ops.push((0, refresh(2, "r2")));
        ops.push((0, refresh(1, "old")));
        ops.push((100, edit(1, "⏹ confirm")));
        let answers = run_timed(&fake, ops).await;
        let calls: Vec<(String, Duration)> = fake
            .calls()
            .iter()
            .skip(5)
            .map(|c| (text_of(&c.op).to_owned(), c.at))
            .collect();
        // It goes in the refresh's place, ahead of every refresh.
        assert_eq!(
            calls,
            [
                ("⏹ confirm".to_owned(), Duration::from_secs(4)),
                ("r2".to_owned(), Duration::from_secs(8))
            ]
        );
        assert!(matches!(answers[6], Some(Ok(Outcome::Superseded))));
        assert!(matches!(answers[7], Some(Ok(Outcome::Done))));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refresh_refused_with_429_gives_way_to_the_newer_edit_of_its_message() {
        let fake = Fake::with_delay(&[5], ms(100));
        let answers = run_timed(&fake, vec![(0, refresh(1, "old")), (50, edit(1, "new"))]).await;
        let calls: Vec<(String, Duration)> = fake
            .calls()
            .iter()
            .map(|c| (text_of(&c.op).to_owned(), c.at))
            .collect();
        assert_eq!(
            calls,
            [("old".to_owned(), ms(0)), ("new".to_owned(), ms(5100))],
            "the older text never overwrites the newer one"
        );
        assert!(matches!(answers[0], Some(Ok(Outcome::Superseded))));
        assert!(matches!(answers[1], Some(Ok(Outcome::Done))));
    }

    /// TASK-061: ids are numbered per chat. Edits, reactions and topics of
    /// the same number in the group and in a private chat never mix.
    fn private() -> Chat {
        Chat::Private(crate::hub::chat::PrivateChat::of_user(7_319_402_518))
    }

    fn in_chat(chat: Chat, mut op: Op) -> Op {
        match &mut op {
            Op::Send { chat: at, .. }
            | Op::Edit { chat: at, .. }
            | Op::Stream { chat: at, .. }
            | Op::React { chat: at, .. } => *at = chat,
            _ => unreachable!("not used here"),
        }
        op
    }

    #[tokio::test(start_paused = true)]
    async fn edits_and_reactions_of_one_id_in_two_chats_stay_two() {
        let fake = Fake::new(&[]);
        let react = |chat, emoji: &str| Op::React {
            chat,
            message_id: 7,
            emoji: emoji.to_owned(),
        };
        let results = run(
            &fake,
            vec![
                edit(7, "group a"),
                in_chat(private(), edit(7, "private")),
                edit(7, "group b"),
                react(Chat::Group, "👀"),
                react(private(), "👀"),
                react(Chat::Group, "✍"),
            ],
        )
        .await;
        let calls = fake.calls();
        let edits: Vec<(Chat, &str)> = calls
            .iter()
            .filter_map(|call| match &call.op {
                Op::Edit { chat, text, .. } => Some((*chat, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(edits, [(Chat::Group, "group b"), (private(), "private")]);
        let reactions: Vec<(Chat, &str)> = calls
            .iter()
            .filter_map(|call| match &call.op {
                Op::React { chat, emoji, .. } => Some((*chat, emoji.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(reactions, [(Chat::Group, "✍"), (private(), "👀")]);
        assert!(matches!(results[0], Ok(Outcome::Superseded)));
        assert!(matches!(results[1], Ok(Outcome::Done)));
        assert!(matches!(results[4], Ok(Outcome::Done)));
    }

    #[tokio::test(start_paused = true)]
    async fn topics_of_one_id_in_two_chats_keep_their_own_order_and_stream() {
        // A prompt waits for older messages of its own topic only.
        let fake = Fake::new(&[]);
        run(
            &fake,
            vec![
                send(7, "group"),
                in_chat(private(), permission(7, "private prompt")),
            ],
        )
        .await;
        assert_eq!(texts(&fake), ["private prompt", "group"]);

        // A refused line breaks the stream of its own topic only.
        let fake = Fake::refusing("g-1");
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let handle = tokio::spawn(scheduler.run());
        let mut answers = Vec::new();
        for op in [
            stream_op(1, "g-0", true),
            stream_op(1, "g-1", false),
            stream_op(1, "g-2", false),
            in_chat(private(), stream_op(1, "p-0", false)),
        ] {
            answers.push(outbox.submit(op).await);
        }
        let mut delivered = Vec::new();
        for answer in answers {
            delivered.push(answer.await.ok());
        }
        drop(outbox);
        assert!(handle.await.is_ok());
        assert!(delivered[2].is_none(), "g-2 dropped with its broken stream");
        assert!(
            matches!(delivered[3], Some(Ok(Outcome::Sent(_)))),
            "the private topic 1 goes on"
        );
    }

    // ------------------------------------------------------------ TASK-062

    /// A turn line written into message `message` of topic 1: `text` is
    /// the message's whole new text.
    fn into(message: i64, text: &str) -> Op {
        match line(1, text) {
            Op::Stream {
                chat,
                thread_id,
                text,
                html,
                merge,
                restart,
                notify,
                ..
            } => Op::Stream {
                chat,
                thread_id,
                text,
                html,
                merge,
                restart,
                notify,
                into: Some(message),
            },
            other => other,
        }
    }

    #[test]
    fn a_stream_message_into_a_message_posts_nothing_and_takes_an_edit_token() {
        let op = into(7, "a");
        assert_eq!(op.posts(), None);
        assert!(!op.metered() && op.edit_metered());
        assert_eq!(line(1, "a").posts(), Some(Place::topic(Chat::Group, 1)));
        assert_eq!(send(2, "x").posts(), Some(Place::topic(Chat::Group, 2)));
        assert_eq!(edit(7, "x").posts(), None);
    }

    /// Lines written into one message in a burst go as one edit with the
    /// newest text; the others are answered `Merged` once it is in.
    #[tokio::test(start_paused = true)]
    async fn lines_into_one_message_go_as_one_edit_with_the_newest_text() {
        let fake = Fake::new(&[]);
        let answers = run_timed(
            &fake,
            vec![
                (0, into(7, "a ✓")),
                (500, into(7, "a ✓\nb ✓")),
                (1000, into(7, "a ✓\nb ✓\nc ✓")),
            ],
        )
        .await;
        assert_eq!(
            stream_times(&fake),
            [("a ✓\nb ✓\nc ✓".to_owned(), ms(2500))],
            "one edit, 1.5 s after the last line"
        );
        assert!(matches!(answers[0], Some(Ok(Outcome::Sent(_)))));
        assert!(matches!(answers[1], Some(Ok(Outcome::Merged))));
        assert!(matches!(answers[2], Some(Ok(Outcome::Merged))));
    }

    /// With the group's message budget spent, content written into an
    /// existing message still goes on the edit budget; a new message waits.
    /// The two classes alone: the group's budget (TASK-068), which both
    /// take, is spent as soon as the message one is.
    #[tokio::test(start_paused = true)]
    async fn content_into_a_message_does_not_wait_for_the_message_budget() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<(u64, Op)> = (0..6).map(|i| (0, send(2, &format!("s{i}")))).collect();
        ops.push((5500, into(7, "a ✓")));
        run_classes(&fake, ops).await;
        let times = stream_times(&fake);
        let at = |text: &str| times.iter().find(|(t, _)| t == text).map(|(_, at)| *at);
        // s5 needs a refill: 5 burst sends, the next token 4 s after the first.
        assert!(at("s5").unwrap() >= ms(4000), "{times:?}");
        assert_eq!(at("a ✓"), Some(ms(7000)), "only its debounce: {times:?}");
    }

    /// A status refresh still queued for the message that becomes turn
    /// content would overwrite it: it is answered `Superseded` and never goes.
    #[tokio::test(start_paused = true)]
    async fn content_into_a_message_drops_a_refresh_of_it_that_waits() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        // Spend the edit budget so the refresh waits.
        for i in 0..5 {
            drop(outbox.submit(edit(900 + i, "x")).await);
        }
        let waiting = outbox.submit(refresh(7, "💭 Думает")).await;
        let other = outbox.submit(refresh(8, "💭 Думает")).await;
        let content = outbox.submit(into(7, "> go")).await;
        assert!(matches!(waiting.await, Ok(Ok(Outcome::Superseded))));
        assert!(matches!(content.await, Ok(Ok(Outcome::Sent(_)))));
        assert!(matches!(other.await, Ok(Ok(Outcome::Done))));
        drop(outbox);
        assert!(handle.await.is_ok());
        let seven: Vec<Op> = fake
            .calls()
            .into_iter()
            .map(|call| call.op)
            .filter(|op| {
                matches!(
                    op,
                    Op::Edit { message_id: 7, .. } | Op::Stream { into: Some(7), .. }
                )
            })
            .collect();
        assert!(matches!(seven.as_slice(), [Op::Stream { .. }]), "{seven:?}");
    }

    /// Turn content and status refreshes share the budget that foreground
    /// edits leave: with ten status messages refreshed for good, a turn's
    /// lines still show every few tokens, and the refreshes still go.
    #[tokio::test(start_paused = true)]
    async fn turn_content_and_status_refreshes_share_the_edit_budget() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let refreshes = refresh_forever(&outbox, 10);
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut text = String::new();
        let mut waits = Vec::new();
        for step in 0..30 {
            text.push_str(&format!("step {step} ✓\n"));
            let wait = waited(&outbox, into(7, text.trim_end())).await;
            waits.push(wait);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        // With the group's budget (TASK-068) the turn message goes before
        // the refreshes but for one in five: it waits for its debounce, a
        // refresh's token at most and its own.
        let longest = waits.iter().max().copied().unwrap_or_default();
        let bound = DEBOUNCE + GROUP_BUCKET.refill_every * 2;
        assert!(longest <= bound, "{waits:?}");
        let before = refreshes.lock().map(|log| log.len()).unwrap_or(0);
        tokio::time::sleep(Duration::from_secs(60)).await;
        let after = refreshes.lock().map(|log| log.len()).unwrap_or(0);
        assert!(after > before, "refreshes go on");
        handle.abort();
    }

    /// With the edit budget spent, turn content takes spare message tokens
    /// (TASK-062): lines used to be new messages. The two classes alone, as
    /// above.
    #[tokio::test(start_paused = true)]
    async fn turn_content_takes_spare_message_tokens_when_the_edit_budget_is_spent() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<(u64, Op)> = (0..5).map(|i| (0, edit(900 + i, "x"))).collect();
        for i in 0..5 {
            let mut line = into(10 + i, "a ✓");
            if let Op::Stream { merge, .. } = &mut line {
                *merge = false;
            }
            ops.push((100, line));
        }
        run_classes(&fake, ops).await;
        let at: Vec<Duration> = fake
            .calls()
            .into_iter()
            .filter(|call| matches!(call.op, Op::Stream { .. }))
            .map(|call| call.at)
            .collect();
        assert_eq!(at.len(), 5);
        assert!(at[4] <= ms(5000), "one per message token: {at:?}");
    }

    /// Ten topics whose turn messages grow every 2 s and ten status
    /// messages refreshed for good share the group's budget (TASK-068):
    /// every turn message still shows its new lines within a bound, and
    /// the refreshes still go.
    #[tokio::test(start_paused = true)]
    async fn ten_growing_turn_messages_share_the_group_budget() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let _refreshes = refresh_forever(&outbox, 10);
        let mut writers = Vec::new();
        for topic in 0..10i64 {
            let outbox = outbox.clone();
            writers.push(tokio::spawn(async move {
                let mut text = String::new();
                for step in 0..150 {
                    text.push_str(&format!("{topic}-{step}\n"));
                    let mut op = into(500 + topic, text.trim_end());
                    if let Op::Stream { thread_id, .. } = &mut op {
                        *thread_id = topic;
                    }
                    drop(outbox.submit(op).await);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }));
        }
        tokio::time::sleep(Duration::from_secs(300)).await;
        let calls = fake.calls();
        for topic in 0..10i64 {
            let at: Vec<Duration> = calls
                .iter()
                .filter(
                    |call| matches!(call.op, Op::Stream { into: Some(m), .. } if m == 500 + topic),
                )
                .map(|call| call.at)
                .collect();
            let gap = at
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .max()
                .unwrap_or_default();
            // The ten turn messages take four of every five tokens (a status
            // refresh the fifth), oldest first: each comes around within
            // ceil(10 x 5 / 4) = 13 tokens of 4 s, a debounce after its
            // line at most; 300 s give 5 + 75 tokens, six each.
            let bound = GROUP_BUCKET.refill_every * 13 + DEBOUNCE;
            assert!(at.len() >= 3, "topic {topic}: {} writes", at.len());
            assert!(gap <= bound, "topic {topic}: {gap:?} between writes");
        }
        let refreshes = calls.iter().filter(|call| call.op.background()).count();
        assert!(refreshes >= 10, "{refreshes} refreshes");
        handle.abort();
    }
    /// A delete (an old status message, a service message) keeps its place
    /// among the topic calls: eleven slots editing their topics without a
    /// pause do not hold it back for good (TASK-062 plan review); only a new
    /// topic goes ahead of it.
    #[tokio::test(start_paused = true)]
    async fn a_delete_is_not_held_back_by_topic_edits_queued_after_it() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let mut editors = Vec::new();
        for slot in 0..11i64 {
            let outbox = outbox.clone();
            editors.push(tokio::spawn(async move {
                for i in 0u64.. {
                    let answer = outbox
                        .submit(Op::EditTopic {
                            chat: Chat::Group,
                            thread_id: 10 + slot,
                            name: None,
                            icon_custom_emoji_id: Some(format!("{i}")),
                        })
                        .await;
                    if answer.await.is_err() {
                        return;
                    }
                }
            }));
        }
        tokio::time::sleep(ms(100)).await;
        let delete = waited(
            &outbox,
            Op::Delete {
                chat: Chat::Group,
                message_id: 77,
            },
        )
        .await;
        // Behind the eleven edits queued before it, one token each (48 s
        // measured), where the planner's order never sent it.
        assert!(delete <= Duration::from_secs(60), "{delete:?}");
        let create = waited(
            &outbox,
            Op::CreateTopic {
                chat: Chat::Group,
                name: "new".to_owned(),
                icon_custom_emoji_id: None,
            },
        )
        .await;
        assert!(create <= EDIT_BUCKET.refill_every, "{create:?}");
        for editor in editors {
            editor.abort();
        }
        handle.abort();
    }

    // ------------------------------------------------------------ TASK-068

    /// Ten status messages refreshed for good keep the group's budget busy:
    /// a new message still takes the next token of the group, not the fifth.
    #[tokio::test(start_paused = true)]
    async fn a_new_message_takes_the_groups_token_before_status_refreshes() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
        let handle = tokio::spawn(scheduler.run());
        let refreshes = refresh_forever(&outbox, 10);
        tokio::time::sleep(Duration::from_secs(20)).await;
        for i in 0..6i64 {
            tokio::time::sleep(Duration::from_millis(5300)).await;
            let wait = waited(&outbox, send(i, &format!("m{i}"))).await;
            assert!(wait <= GROUP_BUCKET.refill_every, "m{i} waited {wait:?}");
        }
        assert!(
            refreshes.lock().map(|log| log.len()).unwrap_or(0) > 10,
            "refreshes went on meanwhile"
        );
        handle.abort();
    }

    /// A 429 halves the group's rate: the refused request goes when the
    /// pause ends, the next ones one token per 8 s instead of 4 s, and the
    /// rate comes back by a tenth a minute, full after five minutes.
    #[tokio::test(start_paused = true)]
    async fn a_429_halves_the_groups_rate_and_it_comes_back_slowly() {
        let fake = Fake::new(&[10]);
        let ops = (0..110).map(|i| (0, edit(i, "x"))).collect();
        let answers = run_timed(&fake, ops).await;
        assert!(answers.iter().all(|a| matches!(a, Some(Ok(_)))));
        let at: Vec<Duration> = fake.calls().iter().map(|call| call.at).collect();
        assert_eq!(
            at[..2],
            [ms(0), ms(10_000)],
            "the retry when the pause ends"
        );
        let gap = at[2] - at[1];
        assert!(
            (ms(7_900)..=ms(8_000)).contains(&gap),
            "half the rate after the 429: {gap:?}"
        );
        // Half the rate plus a tenth a minute: full five minutes after the
        // pause, 4 s a token from then.
        let full = ms(10_000) + Duration::from_secs(300);
        let later: Vec<Duration> = at
            .windows(2)
            .filter(|pair| pair[0] >= full)
            .map(|pair| pair[1] - pair[0])
            .collect();
        assert!(!later.is_empty(), "the run lasts past the recovery: {at:?}");
        assert!(
            later
                .iter()
                .all(|gap| *gap <= GROUP_BUCKET.refill_every + ms(1)),
            "full rate again: {later:?}"
        );
        let slow: Vec<Duration> = at
            .windows(2)
            .skip(1)
            .map(|pair| pair[1] - pair[0])
            .collect();
        assert!(
            slow.windows(2).all(|pair| pair[1] <= pair[0] + ms(1)),
            "the gaps only shrink: {slow:?}"
        );
    }
}

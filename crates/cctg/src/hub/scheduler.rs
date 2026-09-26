//! Outbound scheduler: every Bot API write goes through one queue.
//!
//! Queues, checked in this order on every pick:
//! 1. a permission prompt from `Message` that has no older message of its topic
//!    queued (stream lines do not count); metered. With a debounce, the
//!    `merge` lines of its topic queued before it go first, at once, as one
//!    message where they fit (they happened before the prompt).
//! 2. `answerCallbackQuery` from `Edit`; no token.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`,
//!    `pinChatMessage` - and foreground `Edit` - `editMessageText` and
//!    `setMessageReaction` that show what a user did or asked for
//!    (decisions, questions, subagent blocks, reactions, ⏹). When both
//!    wait they take turns, so neither waits behind more than one of the
//!    other, and neither ever waits behind a background edit.
//! 4. background `Edit` - the periodic refresh of a status message
//!    (`Op::Edit::background`): only the edit budget foreground edits left.
//!    Edits and reactions are coalesced per message (the newest text wins,
//!    in the oldest one's place, foreground when either was), so the
//!    background queue holds one edit per message and is served oldest
//!    first: round-robin over the status messages, one slot cannot hog it.
//! 5. `Message` - `sendMessage`, `sendDocument`, `sendPhoto` and transcript
//!    stream lines; metered, one FIFO. Permission prompts live here too, so they never
//!    overtake their own topic's ordinary messages; they do overtake its
//!    stream lines, except its debounced ones (1.).
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
//! each takes a token from a second group bucket, `Limits::edits`, so the
//! requests into the group per minute stay below the sum of both buckets.
//! Callback answers go to the pressing user, not into the group, and take no
//! token: they go ahead of edits that wait for theirs. Under a steady load of
//! background refreshes a lone topic call or foreground edit waits at most
//! for the next edit token (two when the other class waits too).
//! Everything is serialized (one request in flight).
//! Any 429 pauses the whole queue for `retry_after` and puts the job back at
//! the head of its lane; an edit or reaction whose message got a newer one
//! queued meanwhile is answered `Superseded` instead.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use super::api::{ApiError, BotApi, Document, ForumTopic, Message};

const QUEUE_CAPACITY: usize = 1024;
const MAX_CONSECUTIVE_UNMETERED: usize = 4;
const MIN_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub enum Op {
    Send {
        thread_id: Option<i64>,
        /// Plain text; the fallback when `html` is set and refused.
        text: String,
        /// `text` as Telegram HTML (`parse_mode: HTML`), sent instead of it.
        html: Option<String>,
        reply_markup: Option<Value>,
        /// Permission prompts jump ahead of ordinary messages of other topics,
        /// never ahead of older messages of their own topic.
        permission: bool,
        /// The message this one answers (`reply_parameters`).
        reply_to: Option<i64>,
        /// With a sound; `false` sends it with `disable_notification`.
        notify: bool,
    },
    SendDocument {
        thread_id: Option<i64>,
        document: Document,
        /// As in `Send`.
        notify: bool,
    },
    /// `sendPhoto` (TASK-032); a picture Telegram refuses as a photo (400:
    /// dimensions, format) goes as a document in the same job.
    SendPhoto {
        thread_id: Option<i64>,
        document: Document,
        /// As in `Send`.
        notify: bool,
    },
    Edit {
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
        message_id: i64,
    },
    /// `pinChatMessage` without a notification.
    Pin {
        message_id: i64,
    },
    CreateTopic {
        name: String,
        icon_custom_emoji_id: Option<String>,
    },
    EditTopic {
        thread_id: i64,
        name: Option<String>,
        icon_custom_emoji_id: Option<String>,
    },
    /// A message of the live transcript stream (TASK-016). `merge`: a one-line
    /// tool call that may share a message with the lines queued after it.
    /// `restart`: the first line of a stream (again); it ends a break of its
    /// topic's stream.
    Stream {
        thread_id: i64,
        text: String,
        /// As in `Send`.
        html: Option<String>,
        merge: bool,
        restart: bool,
        /// As in `Send`; only lines of equal `notify` share a message.
        notify: bool,
    },
    /// `setMessageReaction` with one emoji; a newer one for the same message
    /// replaces a queued one.
    React {
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
            | Op::Stream { .. } => Lane::Message(0),
            Op::Edit { .. } | Op::AnswerCallback { .. } | Op::React { .. } => Lane::Edit(0),
            Op::Delete { .. } | Op::Pin { .. } | Op::CreateTopic { .. } | Op::EditTopic { .. } => {
                Lane::Topic
            }
        }
    }

    /// Only new messages count against the group message limit.
    fn metered(&self) -> bool {
        matches!(
            self,
            Op::Send { .. } | Op::SendDocument { .. } | Op::SendPhoto { .. } | Op::Stream { .. }
        )
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
            (Op::Edit { message_id: a, .. }, Op::Edit { message_id: b, .. })
            | (Op::React { message_id: a, .. }, Op::React { message_id: b, .. }) => a == b,
            _ => false,
        }
    }

    /// The topic of a new message.
    fn thread(&self) -> Option<Option<i64>> {
        match self {
            Op::Send { thread_id, .. }
            | Op::SendDocument { thread_id, .. }
            | Op::SendPhoto { thread_id, .. } => Some(*thread_id),
            Op::Stream { thread_id, .. } => Some(Some(*thread_id)),
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
                    *thread_id,
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
                thread_id,
                document,
                notify,
            } => self
                .send_document(*thread_id, document, *notify)
                .await
                .map(Outcome::Sent),
            Op::SendPhoto {
                thread_id,
                document,
                notify,
            } => match self.send_photo(*thread_id, document, *notify).await {
                Err(error) if error.is_photo_refusal() => {
                    warn!("telegram did not take a picture as a photo; sending it as a document");
                    self.send_document(*thread_id, document, *notify).await
                }
                sent => sent,
            }
            .map(Outcome::Sent),
            Op::Edit {
                message_id,
                text,
                reply_markup,
                ..
            } => self
                .edit_message_text(*message_id, text, reply_markup.as_ref())
                .await
                .map(|()| Outcome::Done),
            Op::AnswerCallback { query_id, text } => self
                .answer_callback_query(query_id, text.as_deref())
                .await
                .map(|()| Outcome::Done),
            Op::Delete { message_id } => self
                .delete_message(*message_id)
                .await
                .map(|()| Outcome::Done),
            Op::Pin { message_id } => self
                .pin_chat_message(*message_id)
                .await
                .map(|()| Outcome::Done),
            Op::CreateTopic {
                name,
                icon_custom_emoji_id,
            } => self
                .create_forum_topic(name, icon_custom_emoji_id.as_deref())
                .await
                .map(Outcome::Topic),
            Op::EditTopic {
                thread_id,
                name,
                icon_custom_emoji_id,
            } => self
                .edit_forum_topic(*thread_id, name.as_deref(), icon_custom_emoji_id.as_deref())
                .await
                .map(|()| Outcome::Done),
            Op::Stream {
                thread_id,
                text,
                html,
                notify,
                ..
            } => {
                let (text, parse_mode) = formatted(text, html.as_deref());
                self.send_message(Some(*thread_id), text, None, parse_mode, None, *notify)
                    .await
                    .map(Outcome::Sent)
            }
            Op::React { message_id, emoji } => self
                .set_message_reaction(*message_id, emoji)
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
}

impl Bucket {
    fn new(config: BucketConfig, now: Instant) -> Self {
        Self {
            config,
            tokens: f64::from(config.capacity),
            refilled_at: now,
            last_take: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let gained = elapsed.as_secs_f64() / self.config.refill_every.as_secs_f64();
        self.tokens = (self.tokens + gained).min(f64::from(self.config.capacity));
        self.refilled_at = now;
    }

    /// Earliest instant a metered op may go out.
    fn ready_at(&mut self, now: Instant) -> Instant {
        self.refill(now);
        let token_at = if self.tokens >= 1.0 {
            now
        } else {
            now + self.config.refill_every.mul_f64(1.0 - self.tokens)
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
    debounce: Duration,
    debounce_max: Duration,
    paused_until: Option<Instant>,
    consecutive_unmetered: usize,
    /// When topic calls and foreground edits both wait, a topic call goes
    /// next; they take turns.
    topic_turn: bool,
    /// Topics whose stream broke: their lines wait for a `restart` line.
    broken: HashSet<i64>,
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
            debounce: limits.debounce,
            debounce_max: limits.debounce_max,
            paused_until: None,
            consecutive_unmetered: 0,
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
            let (thread_id, permission) = match &job.op {
                Op::Send {
                    thread_id,
                    permission,
                    ..
                } => (*thread_id, *permission),
                Op::SendDocument { thread_id, .. } | Op::SendPhoto { thread_id, .. } => {
                    (*thread_id, false)
                }
                // Stream lines yield to a prompt of their own topic, except
                // tool-call lines the debounce holds: those came first.
                Op::Stream {
                    thread_id, merge, ..
                } => {
                    first_lines
                        .entry(Some(*thread_id))
                        .or_insert((index, *merge));
                    continue;
                }
                _ => continue,
            };
            if permission && !busy_topics.contains(&thread_id) {
                return match first_lines.get(&thread_id) {
                    Some(&(line, true)) if !self.debounce.is_zero() => Some(line),
                    _ => Some(index),
                };
            }
            busy_topics.insert(thread_id);
        }
        None
    }

    fn enqueue(&mut self, job: Job) {
        if let Op::Stream {
            thread_id, restart, ..
        } = &job.op
        {
            if *restart {
                self.broken.remove(thread_id);
            } else if self.broken.contains(thread_id) {
                // Dropped unsent: its receiver closes without an answer.
                return;
            }
        }
        if let Op::React { message_id, emoji } = &job.op
            && let Some(queued) = self.edit.iter_mut().find(
                |queued| matches!(queued.op, Op::React { message_id: id, .. } if id == *message_id),
            )
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
            message_id,
            text,
            reply_markup,
            background,
        } = &job.op
        {
            let pending = self.edit.iter_mut().find(
                |queued| matches!(queued.op, Op::Edit { message_id: id, .. } if id == *message_id),
            );
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
                let superseded = std::mem::replace(&mut queued.reply, job.reply);
                let _ = superseded.send(Ok(Outcome::Superseded));
                return;
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
        let message_ready = (!self.message.is_empty()).then(|| self.bucket.ready_at(now));
        let permission = self.next_permission();
        if let (Some(index), Some(ready)) = (permission, message_ready)
            && ready <= now
        {
            return Pick::Now(Lane::Message(index));
        }
        let message = self
            .next_message(now)
            .zip(message_ready)
            .map(|((due, index), ready)| (due.max(ready), index));
        if let Some((at, index)) = message
            && at <= now
            && self.consecutive_unmetered >= MAX_CONSECUTIVE_UNMETERED
        {
            return Pick::Now(Lane::Message(index));
        }
        let edit_ready = self.edit_bucket.as_mut().map_or(now, |b| b.ready_at(now));
        if let Some(lane) = self.next_edit(edit_ready <= now) {
            return Pick::Now(lane);
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

    /// The unmetered job to send: the oldest callback answer (no token), and
    /// with a token a topic call or the oldest foreground edit (in turns when
    /// both wait), else the oldest background one.
    fn next_edit(&self, bucket_ready: bool) -> Option<Lane> {
        if let Some(index) = self.edit.iter().position(|job| !job.op.edit_metered()) {
            return Some(Lane::Edit(index));
        }
        if !bucket_ready {
            return None;
        }
        let foreground = self.edit.iter().position(|job| !job.op.background());
        match (self.topic.is_empty(), foreground) {
            (false, Some(index)) if !self.topic_turn => Some(Lane::Edit(index)),
            (false, _) => Some(Lane::Topic),
            (true, Some(index)) => Some(Lane::Edit(index)),
            (true, None) => (!self.edit.is_empty()).then_some(Lane::Edit(0)),
        }
    }

    /// The message to send next and when its debounce allows it: the first
    /// one that has no older message of its topic queued and is due, else
    /// the one due soonest.
    fn next_message(&self, now: Instant) -> Option<(Instant, usize)> {
        let mut topics = HashSet::new();
        let mut soonest: Option<(Instant, usize)> = None;
        for (index, job) in self.message.iter().enumerate() {
            if !topics.insert(job.op.thread()) {
                continue;
            }
            let due = self.due(index);
            if due <= now {
                return Some((due, index));
            }
            if soonest.is_none_or(|(at, _)| due < at) {
                soonest = Some((due, index));
            }
        }
        soonest
    }

    /// When the message at `index`, the first of its topic in the queue, may
    /// go: a `merge` line waits for a quiet `debounce` after the last line
    /// that would join it, at most `debounce_max` after it came, and not at
    /// all once a message of its topic that cannot join (a prompt included)
    /// is queued; anything else at once.
    fn due(&self, index: usize) -> Instant {
        let job = &self.message[index];
        let Op::Stream {
            thread_id,
            merge: true,
            notify,
            ..
        } = &job.op
        else {
            return job.queued_at;
        };
        if self.debounce.is_zero() || job.plain_retry {
            return job.queued_at;
        }
        let mut last = job.queued_at;
        for later in self.message.iter().skip(index + 1) {
            if later.op.thread() != Some(Some(*thread_id)) {
                continue;
            }
            match &later.op {
                Op::Stream {
                    merge: true,
                    notify: later_notify,
                    ..
                } if later_notify == notify => last = later.queued_at,
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
            Lane::Topic => 0,
        };
        let Some(mut job) = self.lane_mut(lane).remove(index) else {
            return;
        };
        if matches!(lane, Lane::Message(_)) {
            self.merge_lines(&mut job, Instant::now());
        }
        if job.op.metered() {
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
                warn!(?wait, "telegram flood control, outbound queue paused");
                self.paused_until = Some(Instant::now() + wait);
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
                if let (Op::Stream { thread_id, .. }, Err(error)) = (&job.op, &result)
                    && !matches!(error, ApiError::Telegram { code, .. } if (400..500).contains(code))
                {
                    self.break_stream(*thread_id);
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

    /// Drops the queued lines of `thread_id`'s stream up to its next
    /// `restart` line, and every later one until such a line comes.
    fn break_stream(&mut self, thread_id: i64) {
        self.broken.insert(thread_id);
        let mut broken = true;
        self.message.retain(|job| match &job.op {
            Op::Stream {
                thread_id: thread,
                restart,
                ..
            } if *thread == thread_id => {
                broken &= !*restart;
                !broken
            }
            _ => true,
        });
        if !broken {
            self.broken.remove(&thread_id);
        }
    }

    /// Joins the stream lines of `job`'s topic queued right after it into
    /// its text: always with a debounce, else when more messages wait than
    /// the bucket has tokens.
    fn merge_lines(&mut self, job: &mut Job, now: Instant) {
        if job.plain_retry {
            return;
        }
        let Op::Stream {
            thread_id,
            text,
            html,
            merge: true,
            notify,
            ..
        } = &mut job.op
        else {
            return;
        };
        self.bucket.refill(now);
        if self.debounce.is_zero() && (self.message.len() + 1) as f64 <= self.bucket.tokens {
            return;
        }
        let mut index = 0;
        while index < self.message.len() {
            let queued = &self.message[index].op;
            if queued.thread() != Some(Some(*thread_id)) {
                index += 1;
                continue;
            }
            let Op::Stream {
                text: next,
                html: next_html,
                merge: true,
                notify: next_notify,
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
            message_id,
            text: text.to_owned(),
            reply_markup: None,
            background: false,
        }
    }

    /// A periodic status refresh.
    fn refresh(message_id: i64, text: &str) -> Op {
        Op::Edit {
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
                name: format!("t{i}"),
                icon_custom_emoji_id: None,
            });
            ops.push(Op::EditTopic {
                thread_id: i,
                name: None,
                icon_custom_emoji_id: Some("5".to_owned()),
            });
            ops.push(Op::Delete { message_id: i });
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
            thread_id: thread,
            text: text.to_owned(),
            html: None,
            merge: true,
            restart: false,
            notify: false,
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
            thread_id: thread,
            text: text.to_owned(),
            html: None,
            merge: false,
            restart,
            notify: false,
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
                thread_id: 1,
                text: format!("s{i}"),
                html: None,
                merge: false,
                restart: false,
                notify: false,
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
                Op::React { message_id, emoji } => Some((*message_id, emoji.clone(), call.at)),
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
                thread_id: 1,
                text: "_b_".to_owned(),
                html: Some("<i>b</i>".to_owned()),
                merge: false,
                restart: true,
                notify: false,
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
            thread_id: 1,
            text: "\u{1F4AD} **x**".to_owned(),
            html: Some("\u{1F4AD} <b>x</b>".to_owned()),
            merge: true,
            restart: false,
            notify: false,
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
                thread_id: 1,
                text: format!("**l{i}**"),
                html: Some(format!("<b>l{i}</b>")),
                merge: true,
                restart: false,
                notify: false,
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
        let (scheduler, outbox) = Scheduler::new(fake.clone(), Limits::default());
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
                    message_id: 1000 + i,
                    emoji: "👀".to_owned(),
                },
            ));
            ops.push((
                0,
                Op::EditTopic {
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
                window.len() <= 40,
                "{} requests after {:?}",
                window.len(),
                call.at
            );
        }
        // Messages are not held back by the edits waiting for their bucket.
        let sends: Vec<Duration> = calls
            .iter()
            .filter(|c| c.op.metered())
            .map(|c| c.at)
            .take(5)
            .collect();
        assert_eq!(sends, (0..5).map(Duration::from_secs).collect::<Vec<_>>());
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
                name: "new session".to_owned(),
                icon_custom_emoji_id: None,
            },
            edit(900, "✅ Разрешено"),
            Op::EditTopic {
                thread_id: 5,
                name: None,
                icon_custom_emoji_id: Some("5".to_owned()),
            },
            Op::React {
                message_id: 901,
                emoji: "👀".to_owned(),
            },
            Op::Pin { message_id: 902 },
            Op::Delete { message_id: 903 },
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
                    .submit(Op::Pin {
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
                Op::Pin { message_id } if *message_id >= 900 => Some('T'),
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
}

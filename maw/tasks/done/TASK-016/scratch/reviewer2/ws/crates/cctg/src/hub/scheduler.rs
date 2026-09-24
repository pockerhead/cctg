//! Outbound scheduler: every Bot API write goes through one queue.
//!
//! Queues, checked in this order on every pick:
//! 1. a permission prompt from `Message` that has no older message of its topic
//!    queued (stream lines do not count); metered.
//! 2. `Edit` - `editMessageText` and `setMessageReaction` (both coalesced per
//!    message) and `answerCallbackQuery`.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`.
//! 4. `Message` - `sendMessage`, `sendDocument` and transcript stream lines;
//!    metered, one FIFO. Permission prompts live here too, so they never
//!    overtake their own topic's ordinary messages; they do overtake its
//!    stream lines.
//!
//! Stream lines marked `merge` (one tool call each) go out one per message
//! while the group budget has room. When more messages wait than there are
//! tokens, the head line takes the lines of its topic queued right after it
//! (up to the first other message of that topic) into one message, in order,
//! while it fits Telegram's limit.
//!
//! A ready ordinary message is served after a bounded run of unmetered jobs,
//! while a ready permission prompt always remains first.
//!
//! Metered ops take a token from the group bucket. Edits and topic mutations
//! have no published limit, so they are only serialized (one request in flight)
//! and paused by 429. Any 429 pauses the whole queue for `retry_after` and puts
//! the job back at the head of its lane.

use std::collections::{HashSet, VecDeque};
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
        text: String,
        reply_markup: Option<Value>,
        /// Permission prompts jump ahead of ordinary messages of other topics,
        /// never ahead of older messages of their own topic.
        permission: bool,
    },
    SendDocument {
        thread_id: Option<i64>,
        document: Document,
    },
    Edit {
        message_id: i64,
        text: String,
        reply_markup: Option<Value>,
    },
    AnswerCallback {
        query_id: String,
        text: Option<String>,
    },
    Delete {
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
    Stream {
        thread_id: i64,
        text: String,
        merge: bool,
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
    Edit,
    Topic,
    /// Index of the job to send in the `Message` queue.
    Message(usize),
}

impl Op {
    fn lane(&self) -> Lane {
        match self {
            Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. } => Lane::Message(0),
            Op::Edit { .. } | Op::AnswerCallback { .. } | Op::React { .. } => Lane::Edit,
            Op::Delete { .. } | Op::CreateTopic { .. } | Op::EditTopic { .. } => Lane::Topic,
        }
    }

    /// Only new messages count against the group message limit.
    fn metered(&self) -> bool {
        matches!(
            self,
            Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. }
        )
    }

    /// The topic of a new message.
    fn thread(&self) -> Option<Option<i64>> {
        match self {
            Op::Send { thread_id, .. } | Op::SendDocument { thread_id, .. } => Some(*thread_id),
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
                reply_markup,
                ..
            } => self
                .send_message(*thread_id, text, reply_markup.as_ref())
                .await
                .map(Outcome::Sent),
            Op::SendDocument {
                thread_id,
                document,
            } => self
                .send_document(*thread_id, document)
                .await
                .map(Outcome::Sent),
            Op::Edit {
                message_id,
                text,
                reply_markup,
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
                thread_id, text, ..
            } => self
                .send_message(Some(*thread_id), text, None)
                .await
                .map(Outcome::Sent),
            Op::React { message_id, emoji } => self
                .set_message_reaction(*message_id, emoji)
                .await
                .map(|()| Outcome::Done),
        }
    }
}

/// Token bucket for new messages in the group.
///
/// Sends in any 60 s window are at most `capacity + 60 s / refill_every`,
/// which the defaults keep at 20. `min_gap` keeps ~1 message/s per chat.
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
    reply: oneshot::Sender<Delivery>,
    /// Stream lines sent inside this job's message.
    merged: Vec<oneshot::Sender<Delivery>>,
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
                reply,
                merged: Vec::new(),
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
    paused_until: Option<Instant>,
    consecutive_unmetered: usize,
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
    pub fn new(transport: Arc<T>, bucket: BucketConfig) -> (Self, Outbox) {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let scheduler = Self {
            transport,
            rx,
            open: true,
            bucket: Bucket::new(bucket, Instant::now()),
            paused_until: None,
            consecutive_unmetered: 0,
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
            Lane::Edit => &mut self.edit,
            Lane::Topic => &mut self.topic,
            Lane::Message(_) => &mut self.message,
        }
    }

    /// The oldest queued permission prompt with no older message of its topic.
    fn next_permission(&self) -> Option<usize> {
        let mut busy_topics = HashSet::new();
        for (index, job) in self.message.iter().enumerate() {
            let (thread_id, permission) = match &job.op {
                Op::Send {
                    thread_id,
                    permission,
                    ..
                } => (*thread_id, *permission),
                Op::SendDocument { thread_id, .. } => (*thread_id, false),
                // Stream lines yield to a prompt of their own topic.
                _ => continue,
            };
            if permission && !busy_topics.contains(&thread_id) {
                return Some(index);
            }
            busy_topics.insert(thread_id);
        }
        None
    }

    fn enqueue(&mut self, job: Job) {
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
        } = &job.op
        {
            let pending = self.edit.iter_mut().find(
                |queued| matches!(queued.op, Op::Edit { message_id: id, .. } if id == *message_id),
            );
            if let Some(queued) = pending {
                if let Op::Edit {
                    text: queued_text,
                    reply_markup: queued_markup,
                    ..
                } = &mut queued.op
                {
                    queued_text.clone_from(text);
                    queued_markup.clone_from(reply_markup);
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
        if message_ready.is_some_and(|ready| ready <= now)
            && self.consecutive_unmetered >= MAX_CONSECUTIVE_UNMETERED
        {
            return Pick::Now(Lane::Message(0));
        }
        if !self.edit.is_empty() {
            return Pick::Now(Lane::Edit);
        }
        if !self.topic.is_empty() {
            return Pick::Now(Lane::Topic);
        }
        match message_ready {
            Some(ready) if ready <= now => Pick::Now(Lane::Message(permission.unwrap_or(0))),
            Some(ready) => Pick::At(ready),
            None => Pick::Idle,
        }
    }

    async fn dispatch(&mut self, lane: Lane) {
        let index = match lane {
            Lane::Message(index) => index,
            Lane::Edit | Lane::Topic => 0,
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
            self.consecutive_unmetered = self.consecutive_unmetered.saturating_add(1);
        }
        match self.transport.execute(&job.op).await {
            Err(ApiError::RetryAfter(wait)) => {
                let wait = wait.max(MIN_RETRY_AFTER);
                warn!(?wait, "telegram flood control, outbound queue paused");
                self.paused_until = Some(Instant::now() + wait);
                // Safe for a prompt taken from the middle: nothing older of
                // its topic was queued, so the head keeps every topic's order.
                self.lane_mut(lane).push_front(job);
            }
            result => {
                let accepted = result.is_ok();
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

    /// Joins the stream lines of `job`'s topic queued right after it into
    /// its text when more messages wait than the bucket has tokens.
    fn merge_lines(&mut self, job: &mut Job, now: Instant) {
        let Op::Stream {
            thread_id,
            text,
            merge: true,
        } = &mut job.op
        else {
            return;
        };
        self.bucket.refill(now);
        if (self.message.len() + 1) as f64 <= self.bucket.tokens {
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
                merge: true,
                ..
            } = queued
            else {
                break;
            };
            if transcript::telegram_len(text) + 1 + transcript::telegram_len(next)
                > transcript::TELEGRAM_TEXT_LIMIT
            {
                break;
            }
            text.push('\n');
            text.push_str(next);
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
    /// first `flood` calls and a 502 for a message containing `refuse`.
    struct Fake {
        start: Instant,
        calls: Mutex<Vec<Call>>,
        flood: Mutex<VecDeque<Duration>>,
        delay: Duration,
        refuse: Option<&'static str>,
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
            })
        }

        fn refusing(text: &'static str) -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                calls: Mutex::new(Vec::new()),
                flood: Mutex::new(VecDeque::new()),
                delay: Duration::ZERO,
                refuse: Some(text),
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
                    code: 502,
                    description: "Bad Gateway".to_owned(),
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
            reply_markup: None,
            permission: false,
        }
    }

    fn edit(message_id: i64, text: &str) -> Op {
        Op::Edit {
            message_id,
            text: text.to_owned(),
            reply_markup: None,
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

    /// Enqueues everything first, then runs the scheduler to completion.
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
            reply_markup: None,
            permission: true,
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
            reply_markup: None,
            permission: true,
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
            merge: true,
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
        for (i, answer) in answers.iter().enumerate() {
            let in_refused = refused.split('\n').any(|line| line == format!("t-{i}"));
            match answer {
                Some(Ok(Outcome::Sent(_) | Outcome::Merged)) => assert!(!in_refused, "t-{i}"),
                Some(Err(_)) | None => assert!(in_refused, "t-{i}: {answer:?}"),
                other => panic!("t-{i}: {other:?}"),
            }
        }
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
                merge: false,
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
}

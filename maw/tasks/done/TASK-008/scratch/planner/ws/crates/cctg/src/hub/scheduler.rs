//! Outbound scheduler: every Bot API write goes through one queue.
//!
//! Lanes, checked in this order on every pick:
//! 1. `Permission` - permission prompts; metered.
//! 2. `Edit` - `editMessageText` (coalesced per message) and `answerCallbackQuery`.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`.
//! 4. `Message` - `sendMessage`, `sendDocument`; metered, one FIFO.
//!
//! Metered ops take a token from the group bucket. Edits and topic mutations
//! have no published limit, so they are only serialized (one request in flight)
//! and paused by 429. Any 429 pauses the whole queue for `retry_after` and puts
//! the job back at the head of its lane.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use super::api::{ApiError, BotApi, Document, ForumTopic, Message};

const QUEUE_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub enum Op {
    Send {
        thread_id: Option<i64>,
        text: String,
        reply_markup: Option<Value>,
        /// Permission prompts jump ahead of ordinary messages.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Permission,
    Edit,
    Topic,
    Message,
}

const LANES: [Lane; 4] = [Lane::Permission, Lane::Edit, Lane::Topic, Lane::Message];

impl Op {
    fn lane(&self) -> Lane {
        match self {
            Op::Send {
                permission: true, ..
            } => Lane::Permission,
            Op::Send { .. } | Op::SendDocument { .. } => Lane::Message,
            Op::Edit { .. } | Op::AnswerCallback { .. } => Lane::Edit,
            Op::Delete { .. } | Op::CreateTopic { .. } | Op::EditTopic { .. } => Lane::Topic,
        }
    }

    /// Only new messages count against the group message limit.
    fn metered(&self) -> bool {
        matches!(self, Op::Send { .. } | Op::SendDocument { .. })
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Sent(Message),
    Topic(ForumTopic),
    Done,
    /// A newer edit of the same message replaced this one before it was sent.
    Superseded,
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
        let _ = self.tx.send(Job { op, reply }).await;
        receiver
    }
}

pub struct Scheduler<T> {
    transport: Arc<T>,
    rx: mpsc::Receiver<Job>,
    open: bool,
    bucket: Bucket,
    paused_until: Option<Instant>,
    permission: VecDeque<Job>,
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
            permission: VecDeque::new(),
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
            Lane::Permission => &mut self.permission,
            Lane::Edit => &mut self.edit,
            Lane::Topic => &mut self.topic,
            Lane::Message => &mut self.message,
        }
    }

    fn enqueue(&mut self, job: Job) {
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
        let mut earliest: Option<Instant> = None;
        for lane in LANES {
            let Some(head) = self.lane_mut(lane).front() else {
                continue;
            };
            if !head.op.metered() {
                return Pick::Now(lane);
            }
            let ready = self.bucket.ready_at(now);
            if ready <= now {
                return Pick::Now(lane);
            }
            earliest = Some(earliest.map_or(ready, |at| at.min(ready)));
        }
        earliest.map_or(Pick::Idle, Pick::At)
    }

    async fn dispatch(&mut self, lane: Lane) {
        let Some(job) = self.lane_mut(lane).pop_front() else {
            return;
        };
        if job.op.metered() {
            self.bucket.take(Instant::now());
        }
        match self.transport.execute(&job.op).await {
            Err(ApiError::RetryAfter(wait)) => {
                warn!(?wait, "telegram flood control, outbound queue paused");
                self.paused_until = Some(Instant::now() + wait);
                self.lane_mut(lane).push_front(job);
            }
            result => {
                let _ = job.reply.send(result);
            }
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
    /// first `flood` calls.
    struct Fake {
        start: Instant,
        calls: Mutex<Vec<Call>>,
        flood: Mutex<VecDeque<Duration>>,
    }

    impl Fake {
        fn new(flood: &[u64]) -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                calls: Mutex::new(Vec::new()),
                flood: Mutex::new(flood.iter().copied().map(Duration::from_secs).collect()),
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
            let flood = self.flood.lock().ok().and_then(|mut f| f.pop_front());
            if let Some(wait) = flood {
                return Err(ApiError::RetryAfter(wait));
            }
            Ok(match op {
                Op::CreateTopic { .. } => Outcome::Topic(ForumTopic::default()),
                Op::Send { .. } | Op::SendDocument { .. } => Outcome::Sent(Message::default()),
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
    async fn stops_after_outbox_is_dropped_and_queue_drained() {
        let fake = Fake::new(&[]);
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let handle = tokio::spawn(scheduler.run());
        let receiver = outbox.submit(send(1, "x")).await;
        drop(outbox);
        assert!(matches!(receiver.await, Ok(Ok(Outcome::Sent(_)))));
        assert!(handle.await.is_ok());
    }
}

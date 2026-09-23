//! Log capture for the cap on messages waiting for Telegram: one warning per
//! overflow episode, armed again once the backlog is gone. Its own test
//! binary with a global subscriber, like `slots_logs.rs`: the actor runs on
//! runtime workers and parallel tests would race on tracing callsite
//! registration.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, MAX_QUEUED_MESSAGES, Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{HookEvent, HookPost};
use tokio::sync::mpsc;

const OVERFLOW: &str = "too many messages wait for Telegram";

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

impl Captured {
    fn count(&self, needle: &str) -> usize {
        let logs = self.0.lock().map(|l| l.clone()).unwrap_or_default();
        String::from_utf8_lossy(&logs).matches(needle).count()
    }
}

/// Topic 100 for the first create; sends wait while the gate is closed.
#[derive(Default)]
struct Gated {
    open: AtomicBool,
    sends_seen: AtomicUsize,
    sends_answered: AtomicUsize,
}

impl Transport for Gated {
    async fn execute(&self, op: &Op) -> Delivery {
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } => {
                self.sends_seen.fetch_add(1, Ordering::SeqCst);
                while !self.open.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                self.sends_answered.fetch_add(1, Ordering::SeqCst);
                Ok(Outcome::Sent(Message::default()))
            }
            _ => Ok(Outcome::Done),
        }
    }
}

async fn until(what: &str, done: impl Fn() -> bool) {
    let reached = async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), reached)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
    // Let the actor finish whatever it was handed before.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_overflow_warning_per_episode() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let state = std::env::temp_dir().join(format!("cctg-overflow-logs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");

    let fake = Arc::new(Gated::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).expect("store");
    let options = Options {
        grace: Duration::ZERO,
        // Every message to the dead session asks for a notice.
        notice_every: Duration::ZERO,
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().expect("load"), store, outbox, options);
    let (_agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    hooks
        .send(HookPost::new(
            "box".into(),
            "0ff10a00-0000-4000-8000-000000000001".into(),
            r"C:\w\p".into(),
            String::new(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await
        .expect("hook");
    let say = |message_id: i64| {
        control
            .send(Control::Message(Inbound {
                message_id,
                thread_id: Some(100),
                text: Some("x".into()),
                reply_to: None,
            }))
            .expect("control");
    };
    // The session has no agent: every message becomes an offline notice.
    // Resend until the topic is known and the first notice is out.
    until("the first notice", || {
        say(0);
        fake.sends_seen.load(Ordering::SeqCst) > 0
    })
    .await;

    // First episode: fill the backlog, then several refusals, one warning.
    let mut next = 1;
    for _ in 0..MAX_QUEUED_MESSAGES + 5 {
        say(next);
        next += 1;
    }
    until("the first warning", || captured.count(OVERFLOW) >= 1).await;
    assert_eq!(captured.count(OVERFLOW), 1);

    // Telegram answers everything: the backlog drains to zero.
    fake.open.store(true, Ordering::SeqCst);
    until("the drain", || {
        fake.sends_answered.load(Ordering::SeqCst) >= MAX_QUEUED_MESSAGES
    })
    .await;
    assert_eq!(captured.count(OVERFLOW), 1, "no warning while draining");

    // Second episode: warned once more.
    fake.open.store(false, Ordering::SeqCst);
    for _ in 0..MAX_QUEUED_MESSAGES + 5 {
        say(next);
        next += 1;
    }
    until("the second warning", || captured.count(OVERFLOW) >= 2).await;
    assert_eq!(captured.count(OVERFLOW), 2);
    let _ = std::fs::remove_dir_all(&state);
}

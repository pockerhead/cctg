//! Log capture for the status message (TASK-029): a status message that
//! Telegram never takes is warned about once per failure episode, not on
//! every retry. Its own test binary with a global subscriber (parallel tests
//! race on tracing callsite registration).

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use cctg::wire::{HookEvent, HookPost};
use tokio::sync::mpsc;

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

/// Creates topic 100; refuses every message that carries a keyboard (the
/// status message), takes the rest.
#[derive(Default)]
struct NoStatus(Mutex<Vec<Op>>);

impl Transport for NoStatus {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().expect("ops").push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send {
                reply_markup: Some(_),
                ..
            } => Err(ApiError::Telegram {
                code: 403,
                description: "Forbidden: not enough rights".to_owned(),
            }),
            _ => Ok(Outcome::Done),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_message_that_keeps_failing_is_warned_about_once() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let state = std::env::temp_dir().join(format!("cctg-status-logs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");
    let fake = Arc::new(NoStatus::default());
    let bucket = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).expect("store");
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(Duration::from_millis(20)),
        retry_every: Duration::from_millis(50),
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().expect("load"), store, outbox, options);
    let (_agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (_control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let session = "5e551017-0000-4000-8000-000000000029";
    let post = |event| {
        HookPost::new(
            "box".into(),
            session.into(),
            "C:/qa/status-logs".into(),
            String::new(),
            event,
        )
    };
    hooks
        .send(post(HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(10),
            parent_claude_pid: None,
        }))
        .await
        .expect("hook");
    // Keep the actor busy with events so it pumps the status often.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(60)).await;
        hooks
            .send(post(HookEvent::UserPromptSubmit { prompt_id: None }))
            .await
            .expect("hook");
    }

    let tries = fake
        .0
        .lock()
        .expect("ops")
        .iter()
        .filter(|op| {
            matches!(
                op,
                Op::Send {
                    reply_markup: Some(_),
                    ..
                }
            )
        })
        .count();
    assert!(tries >= 3, "the status message was tried again: {tries}");
    let _ = std::fs::remove_dir_all(&state);
    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(
        logs.matches("WARN").count(),
        1,
        "one warn for {tries} tries: {logs}"
    );
    assert!(
        logs.contains("status message not sent; retrying later"),
        "{logs}"
    );
}

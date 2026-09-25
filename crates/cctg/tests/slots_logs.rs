//! Log capture for the slot actor: a missing delete right is logged once and
//! the actor keeps working; paths, folder names and titles never reach the
//! logs. Its own test binary with a global subscriber: the actor and its
//! helpers run on runtime workers, and parallel tests would race on tracing
//! callsite registration.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, Register, SessionAnswer, SessionAsk};
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

/// Creates topics from 100; refuses every delete like a bot without
/// `can_delete_messages`.
#[derive(Default)]
struct NoDeleteRight(Mutex<Vec<Op>>);

impl Transport for NoDeleteRight {
    async fn execute(&self, op: &Op) -> Delivery {
        let mut ops = self.0.lock().expect("ops");
        ops.push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Delete { .. } => Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message can't be deleted".to_owned(),
            }),
            Op::Send { .. } => Ok(Outcome::Sent(Message::default())),
            _ => Ok(Outcome::Done),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slot_logs_warn_once_and_carry_no_private_text() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let folder = format!("PrivateFolder{pid}");
    let cwd = format!(r"C:\Users\private-user-{pid}\{folder}");
    let title = format!("Private title {pid}");
    let state = std::env::temp_dir().join(format!("cctg-slots-logs-{pid}"));
    let _ = std::fs::remove_dir_all(&state);
    let transcript = state.join(format!("private-transcript-{pid}.jsonl"));
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::write(
        &transcript,
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"{title}\"}}\n"),
    )
    .expect("transcript");

    let fake = Arc::new(NoDeleteRight::default());
    let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).expect("store");
    let options = Options {
        grace: Duration::ZERO,
        ..Options::default()
    };
    let slots = Slots::new(store.load().expect("load"), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let session = "5e551017-0000-4000-8000-000000000001";
    // The session's agent reads the title from its transcript (TASK-034).
    let (to_agent, mut from_hub) = mpsc::channel(4);
    let answers = agents.clone();
    let agent_title = title.clone();
    tokio::spawn(async move {
        while let Some(msg) = from_hub.recv().await {
            if let HubMsg::SessionRead {
                read_id,
                ask: SessionAsk::Title { from },
                ..
            } = msg
            {
                let answer = SessionAnswer::Title {
                    title: Some(agent_title.clone()),
                    scanned: from + 1,
                };
                let msg = AgentMsg::SessionAnswer { read_id, answer };
                let event = AgentEvent::Message {
                    conn: 1,
                    received_at: std::time::Instant::now(),
                    msg,
                };
                let _ = answers.send(event).await;
            }
        }
    });
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: cwd.clone(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: true,
            },
            to_agent,
        })
        .await
        .expect("agent");
    let post = |event| {
        HookPost::new(
            "box".into(),
            session.into(),
            cwd.clone(),
            transcript.display().to_string(),
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
    hooks
        .send(post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: None,
        }))
        .await
        .expect("hook");
    // An agent of a session nobody announced: it waits, no topic.
    let (to_agent, _to_agent_rx) = mpsc::channel(4);
    agents
        .send(AgentEvent::Registered {
            conn: 9,
            register: Register {
                session_id: "0dd0dd0d-0000-4000-8000-000000000009".into(),
                host: "box".into(),
                cwd: cwd.clone(),
                claude_pid: None,
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
            },
            to_agent,
        })
        .await
        .expect("agent");
    tokio::time::sleep(Duration::from_millis(500)).await;
    for message_id in 1..=3 {
        control
            .send(Control::TopicEdited {
                thread_id: Some(100),
                message_id,
            })
            .expect("control");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let ops = fake.0.lock().expect("ops").clone();
    let deletes = ops
        .iter()
        .filter(|op| matches!(op, Op::Delete { .. }))
        .count();
    assert_eq!(deletes, 3, "every delete was tried: {ops:?}");
    assert!(
        ops.iter().any(
            |op| matches!(op, Op::EditTopic { name: Some(name), .. } if name.contains(&title))
        ),
        "the title reached the topic: {ops:?}"
    );
    let _ = std::fs::remove_dir_all(&state);

    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(
        logs.matches("cannot delete a forum service message")
            .count(),
        1,
        "{logs}"
    );
    assert!(logs.contains("forum topic created"), "{logs}");
    for private in [
        folder.as_str(),
        &format!("private-user-{pid}"),
        title.as_str(),
        "private-transcript",
    ] {
        assert!(!logs.contains(private), "{private} in logs: {logs}");
    }
}

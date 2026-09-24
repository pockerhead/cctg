//! Log capture for topic messages (also kept ones), agent replies and turn
//! answers: their text and the sender's user id never reach the logs, and
//! the kept message's `registry.json` carries no user id. Its own test binary with a
//! global subscriber, like `slots_logs.rs`: the actor runs on runtime
//! workers and parallel tests would race on tracing callsite registration.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::buffer::QUEUED_NOTICE;
use cctg::hub::config::Allowlist;
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{Routed, route_batch};
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, Register};
use serde_json::json;
use tokio::sync::mpsc;

const CHAT: i64 = -1000000000001;
/// Distinctive enough not to appear by chance (logs run `.without_time()`).
const USER: i64 = 7_318_046_259;

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

/// Topic 100 for the first create; every send is accepted.
#[derive(Default)]
struct Accepting(Mutex<Vec<Op>>);

impl Transport for Accepting {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().expect("ops").push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::SendDocument { .. } => Ok(Outcome::Sent(Message::default())),
            _ => Ok(Outcome::Done),
        }
    }
}

fn topic_message(update_id: i64, text: &str) -> Control {
    let update = json!({ "update_id": update_id, "message": {
        "message_id": update_id + 10, "message_thread_id": 100, "is_topic_message": true,
        "date": 1, "text": text,
        "from": { "id": USER, "is_bot": false, "first_name": "x" },
        "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
    }});
    let allowlist: Allowlist = [USER].into_iter().collect();
    let (_, mut routed) = route_batch(vec![update], None, CHAT, &allowlist);
    match routed.remove(0) {
        Routed::Input(input) => Control::Message(input),
        other => panic!("not input: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_logs_carry_no_text_and_no_user_id() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let inbound_text = format!("private inbound {pid}");
    let reply_text = format!("private reply {pid}");
    let offline_text = format!("private offline {pid}");
    let answer_text = format!("private answer {pid}");
    let state = std::env::temp_dir().join(format!("cctg-message-logs-{pid}"));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");

    let fake = Arc::new(Accepting::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        // Fast bucket: the default 1 s gap would hold the reply behind the notice.
        BucketConfig {
            capacity: 100,
            refill_every: Duration::from_millis(10),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).expect("store");
    let options = Options {
        grace: Duration::ZERO,
        chat_id: CHAT,
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().expect("load"), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let session = "5e551017-0000-4000-8000-000000000001";
    let post = |event| {
        HookPost::new(
            "box".into(),
            session.into(),
            r"C:\w\p".into(),
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    // No agent yet: the message is kept and the user is told.
    control
        .send(topic_message(1, &offline_text))
        .expect("control");
    // Control and agent events are separate actor inputs with no order
    // between them: wait for the notice before the agent registers.
    let noticed = async {
        loop {
            let sent = fake
                .0
                .lock()
                .expect("ops")
                .iter()
                .any(|op| matches!(op, Op::Send { text, .. } if text == QUEUED_NOTICE));
            if sent {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), noticed)
        .await
        .expect("queued notice in time");
    let saved = std::fs::read_to_string(state.join("registry.json")).unwrap_or_default();
    assert!(saved.contains(&offline_text), "the kept message is saved");
    assert!(
        !saved.contains(&USER.to_string()),
        "no user id in the saved buffer"
    );
    let (to_agent, mut to_agent_rx) = mpsc::channel(4);
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: r"C:\w\p".into(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
            },
            to_agent,
        })
        .await
        .expect("agent");
    // The agent takes the kept message first.
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("kept message in time");
    assert!(
        matches!(got, Some(HubMsg::Inbound { ref content, .. }) if *content == offline_text),
        "{got:?}"
    );
    control
        .send(topic_message(2, &inbound_text))
        .expect("control");
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("inbound in time");
    assert!(
        matches!(got, Some(HubMsg::Inbound { ref content, .. }) if *content == inbound_text),
        "{got:?}"
    );
    agents
        .send(AgentEvent::Message {
            conn: 1,
            received_at: std::time::Instant::now(),
            msg: AgentMsg::Reply {
                text: reply_text.clone(),
            },
        })
        .await
        .expect("reply");
    hooks
        .send(post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some(answer_text.clone()),
        }))
        .await
        .expect("hook");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = std::fs::remove_dir_all(&state);

    let ops = fake.0.lock().expect("ops").clone();
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Send { text, .. } if *text == reply_text)),
        "{ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Send { text, .. } if *text == answer_text)),
        "{ops:?}"
    );
    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    for expected in [
        "message kept for the slot until a session is on line",
        "kept messages handed to the slot's session",
        "message forwarded to the session agent",
        "agent reply queued",
        "turn answer queued",
    ] {
        assert!(logs.contains(expected), "{expected}: {logs}");
    }
    for private in [
        inbound_text.as_str(),
        reply_text.as_str(),
        answer_text.as_str(),
        offline_text.as_str(),
        "private",
        &USER.to_string(),
    ] {
        assert!(!logs.contains(private), "{private} in logs: {logs}");
    }
}

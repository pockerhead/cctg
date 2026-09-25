//! Permission relay through the real poll classification, the slot actor and
//! the scheduler, with log capture: a stranger's press sends nothing, an
//! allowlisted press sends one verdict that the agent acknowledges, the end
//! of the session closes the next prompt, and neither the request fields,
//! the request or verdict id nor the user id reach the logs. Its own test
//! binary with a global subscriber (see `message_logs.rs`).

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::config::Allowlist;
use cctg::hub::ingress::AgentEvent;
use cctg::hub::permissions;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{Ignored, Routed, route_batch};
use cctg::wire::{AgentMsg, Behavior, HookEvent, HookPost, HubMsg, PermissionRequest, Register};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const CHAT: i64 = -1000000000001;
/// Distinctive enough not to appear by chance (logs run `.without_time()`).
const USER: i64 = 7_318_046_259;
const STRANGER: i64 = 6_402_917_385;
const REQUEST: &str = "qzxwv";
const SECOND_REQUEST: &str = "wvxzq";
const PROMPT_MESSAGE: i64 = 4242;

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

/// Topic 100; every sent message gets id [`PROMPT_MESSAGE`].
#[derive(Default)]
struct Accepting(Mutex<Vec<Op>>);

impl Accepting {
    fn ops(&self) -> Vec<Op> {
        self.0.lock().expect("ops").clone()
    }
}

impl Transport for Accepting {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().expect("ops").push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::SendDocument { .. } => Ok(Outcome::Sent(Message {
                message_id: PROMPT_MESSAGE,
                ..Message::default()
            })),
            _ => Ok(Outcome::Done),
        }
    }
}

fn press(update_id: i64, from: i64) -> Value {
    json!({ "update_id": update_id, "callback_query": {
        "id": format!("q{update_id}"),
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat_instance": "c",
        "data": permissions::callback_data(Behavior::Allow, REQUEST),
        "message": {
            "message_id": PROMPT_MESSAGE, "message_thread_id": 100, "is_topic_message": true,
            "date": 1, "text": "prompt",
            "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
        },
    }})
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let wait = async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_relay_logs_carry_no_request_and_no_user_id() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let tool = format!("PrivateTool{pid}");
    let description = format!("private description {pid}");
    let preview = format!("private preview {pid}");
    let state = std::env::temp_dir().join(format!("cctg-permission-logs-{pid}"));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");

    let fake = Arc::new(Accepting::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
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
    let slots = Slots::new(store.load().expect("load"), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let session = "5e551017-0000-4000-8000-000000000001";
    hooks
        .send(HookPost::new(
            "box".into(),
            session.into(),
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
    until("topic", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::CreateTopic { .. }))
    })
    .await;
    let (to_agent, mut to_agent_rx) = mpsc::channel(4);
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: r"C:\w\p".into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                heartbeat: false,
            },
            to_agent,
        })
        .await
        .expect("agent");
    agents
        .send(AgentEvent::Message {
            conn: 1,
            received_at: std::time::Instant::now(),
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: REQUEST.into(),
                tool_name: tool.clone(),
                description: description.clone(),
                input_preview: preview.clone(),
            }),
        })
        .await
        .expect("request");
    until("prompt", || {
        fake.ops().iter().any(|op| {
            matches!(
                op,
                Op::Send {
                    permission: true,
                    ..
                }
            )
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let allowlist: Allowlist = [USER].into_iter().collect();
    let (_, routed) = route_batch(
        vec![press(1, STRANGER), press(2, USER)],
        None,
        CHAT,
        &allowlist,
    );
    assert_eq!(routed[0], Routed::Ignored(Ignored::NotAllowed));
    for item in routed {
        if let Routed::Callback(input) = item {
            control.send(Control::Callback(input)).expect("control");
        }
    }
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("verdict in time");
    let Some(HubMsg::PermissionVerdict {
        request_id,
        behavior: Behavior::Allow,
        verdict_id: Some(verdict_id),
    }) = got
    else {
        panic!("one allow verdict with an id: {got:?}");
    };
    assert_eq!(request_id, REQUEST);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !fake.ops().iter().any(|op| matches!(op, Op::Edit { .. })),
        "decided only by the ack"
    );
    agents
        .send(AgentEvent::Message {
            conn: 1,
            received_at: std::time::Instant::now(),
            msg: AgentMsg::PermissionAck { verdict_id },
        })
        .await
        .expect("ack");
    until("answer and edit", || {
        fake.ops().iter().any(|op| matches!(op, Op::Edit { .. }))
    })
    .await;
    let answered: Vec<Op> = fake
        .ops()
        .into_iter()
        .filter(|op| matches!(op, Op::AnswerCallback { .. }))
        .collect();
    assert_eq!(
        answered.len(),
        1,
        "the stranger's press is not answered: {answered:?}"
    );
    assert!(to_agent_rx.try_recv().is_err(), "one verdict only");

    // A second prompt is still open when the session ends: it is closed.
    agents
        .send(AgentEvent::Message {
            conn: 1,
            received_at: std::time::Instant::now(),
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: SECOND_REQUEST.into(),
                tool_name: tool.clone(),
                description: description.clone(),
                input_preview: preview.clone(),
            }),
        })
        .await
        .expect("second request");
    let permission_sends = || {
        fake.ops()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            })
            .count()
    };
    until("second prompt", || permission_sends() == 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    hooks
        .send(HookPost::new(
            "box".into(),
            session.into(),
            r"C:\w\p".into(),
            String::new(),
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await
        .expect("hook");
    until("closing edit", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::Edit { text, .. } if text == permissions::CLOSED_TEXT))
    })
    .await;
    let _ = std::fs::remove_dir_all(&state);

    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    for expected in [
        "permission request queued for the topic",
        "permission answer chosen in Telegram",
        "permission verdict forwarded to the session agent",
        "permission verdict taken by the session agent",
        "permission prompt closed: its session ended",
    ] {
        assert!(logs.contains(expected), "{expected}: {logs}");
    }
    for private in [
        tool.as_str(),
        description.as_str(),
        preview.as_str(),
        "private",
        REQUEST,
        SECOND_REQUEST,
        &verdict_id.to_string(),
        &USER.to_string(),
        &STRANGER.to_string(),
    ] {
        assert!(!logs.contains(private), "{private} in logs: {logs}");
    }
}

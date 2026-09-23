use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use cctg::wire::{AgentMsg, HookEvent, HookPost, Register};
use tokio::sync::mpsc;

#[derive(Default)]
struct Fake(Mutex<Vec<Op>>);

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::SendDocument { .. } => {
                Ok(Outcome::Sent(Message::default()))
            }
            _ => Ok(Outcome::Done),
        }
    }
}

fn post(session: &str, event: HookEvent) -> HookPost {
    HookPost::new(
        "box".into(),
        session.into(),
        r"C:\work\p".into(),
        String::new(),
        event,
    )
}

#[tokio::main]
async fn main() {
    let state = std::env::temp_dir().join("task021-repro-reply-state");
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig {
            capacity: 100,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        ..Options::default()
    };
    let (slots, _) = Slots::new(store.load().unwrap(), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (_control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let a = "aaaaaaaa-0000-4000-8000-000000000001";
    let b = "bbbbbbbb-0000-4000-8000-000000000002";
    hooks
        .send(post(
            a,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await
        .unwrap();
    let (to_agent, _from_hub) = mpsc::channel(4);
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: a.into(),
                host: "box".into(),
                cwd: r"C:\work\p".into(),
                claude_pid: Some(10),
            },
            to_agent,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    hooks
        .send(post(
            a,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await
        .unwrap();
    hooks
        .send(post(
            b,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(11),
                parent_claude_pid: None,
            },
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    agents
        .send(AgentEvent::Message {
            conn: 1,
            msg: AgentMsg::Reply {
                text: "late reply from ended A".into(),
            },
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ops = fake.0.lock().unwrap();
    let leaked = ops.iter().any(|op| {
        matches!(op, Op::Send { thread_id: Some(100), text, .. }
            if text == "late reply from ended A")
    });
    println!("late_reply_delivered_to_reused_topic={leaked}");
    assert!(leaked, "the suspected bug did not reproduce");
    drop(ops);
    let _ = std::fs::remove_dir_all(&state);
}

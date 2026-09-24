//! Kept messages of a dead slot over the real agent link (TASK-017): the
//! real `serve_agents` TCP server, the real `Slots` actor and `Scheduler`,
//! a fake Telegram transport and a raw wire peer in place of `cctg agent`
//! (no child process). The session ends, three topic messages are kept, the
//! session is resumed and its agent connects a moment later: the agent gets
//! the three messages in order, once, and the Resume button goes away.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::buffer::{RESUMED_TEXT, resume_text};
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{self, AgentMsg, HookEvent, HookPost, HubMsg, Register, Secret};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SECRET: &str = "buffer-e2e-secret-0123456789";
const SESSION: &str = "0b0ffe20-0000-4000-8000-000000000001";
const CWD: &str = r"C:\w\p";
const WAIT: Duration = Duration::from_secs(30);

/// Topic 100; sent messages numbered from 1000.
#[derive(Default)]
struct Fake(Mutex<(Vec<Op>, i64)>);

impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.0.lock().expect("ops").0.clone()
    }
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        let mut state = self.0.lock().expect("ops");
        state.0.push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } => {
                state.1 += 1;
                Ok(Outcome::Sent(Message {
                    message_id: 999 + state.1,
                    ..Message::default()
                }))
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
    tokio::time::timeout(WAIT, reached)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
}

fn post(event: HookEvent) -> HookPost {
    HookPost::new(
        "box".into(),
        SESSION.into(),
        CWD.into(),
        String::new(),
        event,
    )
}

fn say(message_id: i64, text: &str) -> Control {
    Control::Message(Inbound {
        message_id,
        thread_id: Some(100),
        text: Some(text.into()),
        reply_to: None,
        quote: None,
        forwarded: false,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kept_messages_reach_the_resumed_session_over_tcp_once_in_order() {
    let state = std::env::temp_dir().join(format!("cctg-buffer-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");

    let fake = Arc::new(Fake::default());
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
        chat_id: -1000000000001,
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().expect("load"), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(serve_agents(
        listener,
        Secret::parse(SECRET).expect("secret"),
        agents,
    ));

    let start = |source: &str, pid: u32| {
        post(HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        })
    };
    hooks.send(start("startup", 10)).await.expect("hook");
    until("the topic", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::CreateTopic { .. }))
    })
    .await;
    hooks
        .send(post(HookEvent::SessionEnd {
            reason: None,
            claude_pid: Some(10),
        }))
        .await
        .expect("hook");
    // Hook and control are separate inputs: wait until the end is applied.
    tokio::time::sleep(Duration::from_millis(300)).await;
    for (id, text) in [(1, "first"), (2, "second"), (3, "third")] {
        control.send(say(id, text)).expect("control");
    }
    let button = resume_text(SESSION);
    until("the Resume button", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::Send { text, reply_markup: Some(_), .. } if *text == button))
    })
    .await;

    // The session is resumed; its agent links a moment later over TCP.
    hooks.send(start("resume", 11)).await.expect("hook");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (read, mut write) = TcpStream::connect(addr)
        .await
        .expect("connect")
        .into_split();
    let mut reader = BufReader::new(read);
    let secret = Secret::parse(SECRET).expect("secret");
    write
        .write_all(&wire::encode(&AgentMsg::Hello { secret }))
        .await
        .expect("hello");
    let register = Register {
        session_id: SESSION.into(),
        host: "box".into(),
        cwd: CWD.into(),
        claude_pid: Some(11),
        verdict_ack: true,
        transcript_reads: false,
        console_keys: false,
        console_commands: false,
        client: None,
    };
    write
        .write_all(&wire::encode(&AgentMsg::Register(register)))
        .await
        .expect("register");
    let mut got = Vec::new();
    let mut line = Vec::new();
    while got.len() < 4 {
        line.clear();
        tokio::time::timeout(WAIT, reader.read_until(b'\n', &mut line))
            .await
            .expect("hub line in time")
            .expect("read");
        got.push(wire::decode::<HubMsg>(&line).expect("hub message"));
    }
    assert_eq!(got[0], HubMsg::Registered);
    let kept: Vec<(String, String)> = got[1..]
        .iter()
        .map(|msg| match msg {
            HubMsg::Inbound { content, meta } => (content.clone(), meta["message_id"].clone()),
            other => panic!("not inbound: {other:?}"),
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("first".to_owned(), "1".to_owned()),
            ("second".to_owned(), "2".to_owned()),
            ("third".to_owned(), "3".to_owned())
        ]
    );
    until("the button edited away", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::Edit { text, .. } if text == RESUMED_TEXT))
    })
    .await;
    // Nothing more comes: each kept message went once.
    line.clear();
    let more = tokio::time::timeout(
        Duration::from_millis(500),
        reader.read_until(b'\n', &mut line),
    )
    .await;
    assert!(
        more.is_err(),
        "unexpected line {:?}",
        String::from_utf8_lossy(&line)
    );
    let _ = std::fs::remove_dir_all(&state);
}

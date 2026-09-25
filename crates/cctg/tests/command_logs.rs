//! Log capture for `/brief` and `/full`. Its own test binary with a global
//! subscriber: the slot actor and the agent's reads run on runtime workers
//! and the blocking pool, where a scoped (`with_default`) subscriber would
//! not see them, and parallel tests would race on tracing callsite
//! registration.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::commands::{Asks, handle};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, Register};
use tokio::sync::mpsc;

const SESSION: &str = "5e551017-0000-4000-8000-000000000001";
const DEAD: &str = "dead0000-0000-4000-8000-000000000002";
const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../transcript/tests/fixtures/final_answer.jsonl"
));

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

#[derive(Default)]
struct Fake(Mutex<Vec<Op>>);

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        if let Ok(mut ops) = self.0.lock() {
            ops.push(op.clone());
        }
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            _ => Ok(Outcome::Sent(Message::default())),
        }
    }
}

fn input(text: &str) -> Inbound {
    Inbound {
        message_id: 1,
        thread_id: None,
        text: Some(text.to_owned()),
        reply_to: None,
        quote: None,
        forwarded: false,
        media: None,
    }
}

fn start(session: &str, pid: u32, transcript: &str) -> HookPost {
    HookPost::new(
        "box".into(),
        session.into(),
        r"C:\w\app".into(),
        transcript.into(),
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_logs_carry_no_paths() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let marker = format!("private-marker-{}", std::process::id());
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(&marker);
    let projects = root.join("projects");
    let project = projects.join(format!("C--Users-{marker}-dev"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&project).expect("project dir");
    let transcript = project.join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript, FIXTURE).expect("session file");
    let gone = project.join(format!("{DEAD}.jsonl"));

    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&root).expect("store");
    let options = Options {
        grace: Duration::ZERO,
        ..Options::default()
    };
    let mut slots = Slots::new(store.load().expect("load"), store, outbox.clone(), options);
    let source = Arc::new(Asks(slots.transcript_asks()));
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (_control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    // The session's agent: it reads its files like `cctg agent`.
    let (to_agent, mut from_hub) = mpsc::channel(16);
    let answers = agents.clone();
    let agent_root = cctg::tail::OwnProject::at(project.clone());
    tokio::spawn(async move {
        while let Some(msg) = from_hub.recv().await {
            if let HubMsg::SessionRead {
                read_id,
                session_id,
                ask,
            } = msg
            {
                for answer in cctg::reads::answer(Some(&agent_root), &session_id, ask) {
                    let msg = AgentMsg::SessionAnswer { read_id, answer };
                    let event = AgentEvent::Message {
                        conn: 1,
                        received_at: Instant::now(),
                        msg,
                    };
                    let _ = answers.send(event).await;
                }
            }
        }
    });
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: SESSION.into(),
                host: "box".into(),
                cwd: r"C:\w\app".into(),
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
    hooks
        .send(start(SESSION, 10, &transcript.to_string_lossy()))
        .await
        .expect("hook");
    // A session without an agent, whose file is not there anyway.
    hooks
        .send(start(DEAD, 11, &gone.to_string_lossy()))
        .await
        .expect("hook");
    tokio::time::sleep(Duration::from_millis(300)).await;

    handle(&input("/brief 5e55"), &outbox, &source, None).await;
    handle(&input("/full 1 5e55"), &outbox, &source, None).await;
    handle(&input("/brief dead"), &outbox, &source, None).await;
    handle(&input(&format!("/{marker}")), &outbox, &source, None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = std::fs::remove_dir_all(&root);

    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        logs.contains("transcript command answered") && logs.contains("transcript not available"),
        "expected command logs: {logs}"
    );
    assert!(
        logs.contains("5e551017") && logs.contains("dead0000"),
        "short session ids expected: {logs}"
    );
    assert!(
        logs.contains("unknown slash command"),
        "fixed unknown-command log expected: {logs}"
    );
    assert!(!logs.contains(&marker), "path or project in logs: {logs}");

    let texts: Vec<String> = fake
        .0
        .lock()
        .map(|ops| ops.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|op| match op {
            Op::Send {
                thread_id: None,
                text,
                ..
            } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    // Notices name the session, never the path.
    assert!(texts[2].contains("dead0000"), "{}", texts[2]);
    for text in &texts {
        assert!(!text.contains(&marker), "{text}");
    }
}

//! Log capture for the live transcript stream: a new session's transcript
//! that is not written yet is one debug line (Claude Code creates the file
//! with the first prompt), one deleted later is warned about once while the
//! stream keeps asking, and it goes on once the file is there; a transcript cut below the
//! stream's offset is warned about once and read again from its start. Paths
//! and message text never reach the logs. Its own test binary with a global subscriber (see
//! `slots_logs.rs`).

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use cctg::wire::{HookEvent, HookPost, HubMsg, Register};
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

#[derive(Default)]
struct Recorded(Mutex<Vec<Op>>);

impl Transport for Recorded {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().expect("ops").push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::Stream { .. } => Ok(Outcome::Sent(Message::default())),
            _ => Ok(Outcome::Done),
        }
    }
}

fn streamed(fake: &Recorded) -> Vec<String> {
    fake.0
        .lock()
        .expect("ops")
        .iter()
        .filter_map(|op| match op {
            Op::Stream { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_transcript_is_warned_once_and_the_stream_goes_on() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let session = "5e551017-0000-4000-8000-000000000001";
    let state = std::env::temp_dir().join(format!("cctg-stream-logs-{pid}"));
    let _ = std::fs::remove_dir_all(&state);
    let project = state.join("projects").join(format!("C--private-{pid}"));
    std::fs::create_dir_all(&project).expect("project dir");
    let transcript = project.join(format!("{session}.jsonl"));
    let path = transcript.display().to_string();

    let fake = Arc::new(Recorded::default());
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
        stream_every: Duration::from_millis(10),
        ..Options::default()
    };
    let slots = Slots::new(store.load().expect("load"), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (_control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    hooks
        .send(HookPost::new(
            "box".into(),
            session.into(),
            r"C:\w".into(),
            path.clone(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await
        .expect("hook");
    let (to_agent, mut from_hub) = mpsc::channel(16);
    let reads = agents.clone();
    let root = cctg::tail::OwnProject::at(project.clone());
    let asked = Arc::new(Mutex::new(0usize));
    let counted = asked.clone();
    tokio::spawn(async move {
        while let Some(msg) = from_hub.recv().await {
            if let HubMsg::TranscriptRead {
                session_id, from, ..
            } = msg
            {
                *counted.lock().expect("count") += 1;
                let msg = cctg::tail::read_chunk(Some(&root), &session_id, from);
                let event = AgentEvent::Message {
                    conn: 1,
                    received_at: std::time::Instant::now(),
                    msg,
                };
                if reads.send(event).await.is_err() {
                    return;
                }
            }
        }
    });
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: r"C:\w".into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: true,
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

    // No file yet: many reads, one debug line, no warning.
    let deadline = Instant::now() + Duration::from_secs(60);
    while *asked.lock().expect("count") < 10 {
        assert!(Instant::now() < deadline, "the stream stopped asking");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let secret_text = format!("private prompt {pid}");
    std::fs::write(
        &transcript,
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{secret_text}\"}}}}\n"),
    )
    .expect("transcript");
    while streamed(&fake).is_empty() {
        assert!(Instant::now() < deadline, "the stream never went on");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(streamed(&fake), [format!("> {secret_text}")]);

    // Cut below the offset: one warning, then the file is read from its start.
    std::fs::write(&transcript, "").expect("cut");
    let polls = *asked.lock().expect("count") + 10;
    while *asked.lock().expect("count") < polls {
        assert!(Instant::now() < deadline, "the stream stopped asking");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let after_cut = format!("after the cut {pid}");
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{after_cut}\"}}}}\n"
        ),
    )
    .expect("transcript");
    while streamed(&fake).len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the stream never went on after the cut"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        streamed(&fake),
        [format!("> {secret_text}"), format!("> {after_cut}")]
    );
    // The file goes away after it was read: now it is a warning, once.
    std::fs::remove_file(&transcript).expect("delete");
    let polls = *asked.lock().expect("count") + 10;
    while *asked.lock().expect("count") < polls {
        assert!(Instant::now() < deadline, "the stream stopped asking");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Read the logs before the files go: the stream keeps asking.
    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&state);
    let lines_with =
        |text: &str| -> Vec<&str> { logs.lines().filter(|line| line.contains(text)).collect() };
    let not_yet = lines_with("session transcript not written yet");
    assert_eq!(not_yet.len(), 1, "{logs}");
    assert!(not_yet[0].contains("DEBUG"), "{logs}");
    let not_found = lines_with("session transcript not found");
    assert_eq!(not_found.len(), 1, "{logs}");
    assert!(not_found[0].contains("WARN"), "{logs}");
    assert_eq!(logs.matches("was cut or replaced").count(), 1, "{logs}");
    for private in [
        secret_text.as_str(),
        after_cut.as_str(),
        &format!("C--private-{pid}"),
        "projects",
    ] {
        assert!(!logs.contains(private), "{private} in logs: {logs}");
    }
}

//! Log capture for `/brief` and `/full`. Its own test binary with a global
//! subscriber: file work runs on the blocking pool, where a scoped
//! (`with_default`) subscriber would not see it, and parallel tests would race
//! on tracing callsite registration.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cctg::hub::api::Message;
use cctg::hub::commands::handle;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::sessions::{LocateError, Located, ProjectsDir, TranscriptLocator};
use cctg::hub::updates::Inbound;

const SESSION: &str = "5e551017-0000-4000-8000-000000000001";
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
        Ok(Outcome::Sent(Message::default()))
    }
}

/// Points at a path without checking it, like a stale registry entry would.
struct Fixed(Located);

impl TranscriptLocator for Fixed {
    fn locate(&self, _: Option<i64>, _: Option<&str>) -> Result<Located, LocateError> {
        Ok(self.0.clone())
    }
}

fn input(text: &str) -> Inbound {
    Inbound {
        message_id: 1,
        thread_id: None,
        text: Some(text.to_owned()),
    }
}

#[tokio::test]
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
    let project = format!("C--Users-{marker}-dev");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(&project)).expect("project dir");
    std::fs::write(
        root.join(&project).join(format!("{SESSION}.jsonl")),
        FIXTURE,
    )
    .expect("session file");

    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
    let scheduler = tokio::spawn(scheduler.run());

    let projects = Arc::new(ProjectsDir::new(root.clone()));
    handle(&input("/brief"), &outbox, &projects, None).await;
    handle(&input("/full 1 5e55"), &outbox, &projects, None).await;
    let located = |path: PathBuf| {
        Arc::new(Fixed(Located {
            session_id: SESSION.to_owned(),
            project: project.clone(),
            path,
        }))
    };
    // Missing file, then a directory instead of a file (read error).
    handle(
        &input("/brief"),
        &outbox,
        &located(root.join("gone.jsonl")),
        None,
    )
    .await;
    handle(&input("/brief"), &outbox, &located(root.clone()), None).await;
    drop(outbox);
    scheduler.await.expect("scheduler");
    let _ = std::fs::remove_dir_all(&root);

    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        logs.contains("transcript command answered") && logs.contains("transcript cannot be read"),
        "expected command logs: {logs}"
    );
    assert!(
        logs.contains("5e551017"),
        "short session id expected: {logs}"
    );
    assert!(!logs.contains(&marker), "path or project in logs: {logs}");

    let ops = fake.0.lock().map(|ops| ops.clone()).unwrap_or_default();
    assert_eq!(ops.len(), 4);
    // Notices name the session, never the path.
    for op in &ops[2..] {
        match op {
            Op::Send { text, .. } => assert!(!text.contains(&marker), "{text}"),
            other => panic!("expected a notice, got {other:?}"),
        }
    }
}

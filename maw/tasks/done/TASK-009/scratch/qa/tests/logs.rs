//! QA TASK-009: log capture across poll + commands, own binary, global subscriber.
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, Message};
use cctg::hub::commands::serve;
use cctg::hub::config::Allowlist;
use cctg::hub::offset::OffsetStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::sessions::ProjectsDir;
use cctg::hub::updates::{Inbound, Routed, UpdateSource, poll};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const CHAT: i64 = -1000000000077;
const ALLOWED: i64 = 987654321;
const STRANGER: i64 = 876543219;
const SESSION: &str = "5e551017-0000-4000-8000-000000000001";

#[derive(Clone, Default)]
struct Cap(Arc<Mutex<Vec<u8>>>);
impl io::Write for Cap {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Failing;
impl Transport for Failing {
    async fn execute(&self, op: &Op) -> Delivery {
        match op {
            Op::Send { text, .. } if text.starts_with("Не удалось") => Ok(Outcome::Sent(Message::default())),
            _ => Err(ApiError::Telegram { code: 400, description: "Bad Request: message thread not found".into() }),
        }
    }
}

struct Tg(Vec<Value>);
impl UpdateSource for Tg {
    fn chat_id(&self) -> i64 {
        CHAT
    }
    async fn get_updates(&self, offset: Option<i64>, t: Duration) -> Result<Vec<Value>, ApiError> {
        let b: Vec<Value> = self.0.iter().filter(|u| offset.is_none_or(|o| u["update_id"].as_i64().unwrap() >= o)).cloned().collect();
        if b.is_empty() {
            tokio::time::sleep(t).await;
        }
        Ok(b)
    }
}

fn upd(id: i64, from: i64, text: &str) -> Value {
    json!({"update_id": id, "message": {"message_id": id, "date": 1, "text": text,
        "from": {"id": from, "is_bot": false, "first_name": "x", "username": "privuser"},
        "chat": {"id": CHAT, "type": "supergroup", "is_forum": true}}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_ids_paths_or_message_text_in_logs() {
    let cap = Cap::default();
    let w = cap.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().without_time().with_max_level(tracing::Level::TRACE).with_writer(move || w.clone()).finish(),
    )
    .unwrap();

    let marker = format!("qamarker{}", std::process::id());
    let root = std::env::temp_dir().join(format!("{marker}-root"));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join(format!("C--Users-{marker}-dev"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(format!("{SESSION}.jsonl")),
        "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}\n").unwrap();

    // Offset store that cannot be saved (dir named offset) + stale warnings.
    let state = std::env::temp_dir().join(format!("{marker}-state"));
    let _ = std::fs::remove_dir_all(&state);
    let store = OffsetStore::open(&state).unwrap();
    std::fs::create_dir_all(state.join("offset").join("x")).unwrap();

    let (sched, outbox) = Scheduler::new(Arc::new(Failing), BucketConfig::default());
    tokio::spawn(sched.run());
    let (tx, rx) = mpsc::unbounded_channel::<Inbound>();
    let worker = tokio::spawn(serve(rx, outbox, Arc::new(ProjectsDir::new(root.clone())), Some("bot".into())));

    let src = Tg(vec![
        upd(1, ALLOWED, "/brief"),
        upd(2, STRANGER, &format!("/full {marker}")),
        upd(3, ALLOWED, &format!("/sessions {marker} secret")),
        upd(4, ALLOWED, &format!("hello {marker}")),
        upd(5, ALLOWED, "/brief 3 5e55"),
    ]);
    let allow: Allowlist = [ALLOWED].into_iter().collect();
    let txc = tx.clone();
    let _ = tokio::time::timeout(Duration::from_secs(3), poll(&src, &allow, &store, move |r| {
        if let Routed::Input(i) = r {
            if i.text.as_deref().is_some_and(|t| t.starts_with('/')) {
                let _ = txc.send(i);
            }
        }
    }))
    .await;
    drop(tx);
    tokio::time::timeout(Duration::from_secs(10), worker).await.unwrap().unwrap();

    let logs = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&state);
    println!("log lines: {}", logs.lines().count());
    assert!(logs.contains("transcript command reply failed"), "{logs}");
    assert!(logs.contains("cannot save the getUpdates offset"), "{logs}");
    assert!(logs.contains("unknown slash command"), "{logs}");
    for bad in [marker.as_str(), "987654321", "876543219", "privuser", "Users", "secret", "hello"] {
        assert!(!logs.contains(bad), "{bad} leaked:\n{logs}");
    }
}

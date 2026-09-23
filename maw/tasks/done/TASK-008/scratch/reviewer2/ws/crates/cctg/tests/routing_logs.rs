//! Log capture for inbound routing. It lives in its own test binary on
//! purpose: `tracing` caches callsite interest globally, and a callsite first
//! hit by a parallel test thread before the scoped subscriber exists can stay
//! disabled, which made this check flaky as a unit test.

use std::io;
use std::sync::{Arc, Mutex};

use cctg::hub::config::Allowlist;
use cctg::hub::updates::route_batch;
use serde_json::{Value, json};

const CHAT: i64 = -1000000000001;
const ALLOWED: i64 = 1001;
const STRANGER: i64 = 2002;
const BOT: i64 = 3003;

fn message(from: i64, extra: Value) -> Value {
    let mut message = json!({
        "message_id": 10,
        "message_thread_id": 7,
        "is_topic_message": true,
        "date": 1,
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
    });
    if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    message
}

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

#[test]
fn routing_logs_never_contain_user_ids() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let allowlist: Allowlist = [ALLOWED].into_iter().collect();
    tracing::subscriber::with_default(subscriber, || {
        let batch = vec![
            json!({ "update_id": 1, "message": message(STRANGER, json!({ "text": "x" })) }),
            json!({ "update_id": 2, "message": message(ALLOWED, json!({ "text": "x" })) }),
            json!({ "update_id": 3, "message": message(BOT, json!({ "forum_topic_closed": {} })) }),
            json!({ "update_id": 4, "message": message(STRANGER, json!({ "text": 1 })) }),
        ];
        route_batch(batch, None, CHAT, &allowlist);
    });
    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        logs.contains("update ignored") && logs.contains("forum service message"),
        "expected debug logs: {logs}"
    );
    for id in [STRANGER, ALLOWED, BOT] {
        assert!(!logs.contains(&id.to_string()), "user id in logs: {logs}");
    }
}

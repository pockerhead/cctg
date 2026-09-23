"""Applies the reviewer-2 changes to updates.rs, offset.rs and mod.rs."""
import io, os

HERE = os.path.dirname(os.path.abspath(__file__))
HUB = os.path.join(HERE, 'ws', 'crates', 'cctg', 'src', 'hub')
s = ''


def load(name):
    global s
    s = io.open(os.path.join(HUB, name), encoding='utf-8', newline='').read()


def save(name):
    io.open(os.path.join(HUB, name), 'w', encoding='utf-8', newline='').write(s)


def rep(old, new):
    global s
    assert s.count(old) == 1, (old[:70], s.count(old))
    s = s.replace(old, new)


# ---------------- updates.rs ----------------
load('updates.rs')
rep("""const STALLED_BATCH_BACKOFF: Duration = Duration::from_secs(1);
""", """const STALLED_BATCH_BACKOFF: Duration = Duration::from_secs(1);
/// Waits before the second and third attempt to save the offset (a file held
/// open by a scanner or indexer on Windows makes the rename fail for a moment).
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];
""")
rep("""/// Routes a raw `getUpdates` batch. Returns the next offset and the routed
/// updates. A malformed update is skipped, not fatal, and still advances the
/// offset so it is not fetched again.
pub fn route_batch(
    raw: Vec<Value>,
    offset: Option<i64>,
    chat_id: i64,
    allowlist: &Allowlist,
) -> (Option<i64>, Vec<Routed>) {
    let mut next = offset;
    let mut routed = Vec::with_capacity(raw.len());
    for value in raw {
        if let Some(id) = value.get("update_id").and_then(Value::as_i64) {
            let following = id.saturating_add(1);
            next = Some(next.map_or(following, |current| current.max(following)));
        }""", """/// Routes a raw `getUpdates` batch. Returns the next offset and the routed
/// updates. A malformed update is skipped, not fatal, and still advances the
/// offset so it is not fetched again.
///
/// The next offset is one past the highest `update_id` of this batch, even
/// when that is below `offset`: after a week without updates Telegram picks
/// the next id at random, and keeping the old, higher offset would fetch and
/// handle the same update again on every call.
pub fn route_batch(
    raw: Vec<Value>,
    offset: Option<i64>,
    chat_id: i64,
    allowlist: &Allowlist,
) -> (Option<i64>, Vec<Routed>) {
    let mut highest: Option<i64> = None;
    let mut routed = Vec::with_capacity(raw.len());
    for value in raw {
        if let Some(id) = value.get("update_id").and_then(Value::as_i64) {
            highest = Some(highest.map_or(id, |current| current.max(id)));
        }""")
rep("""        routed.push(item);
    }
    (next, routed)
}""", """        routed.push(item);
    }
    let next = highest.map(|id| id.saturating_add(1)).or(offset);
    (next, routed)
}""")
rep("""/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
///
/// Starts from the offset in `store` and saves each new offset before the
/// batch is handled: a crash in between skips those updates instead of
/// handling them twice (at most once, so a command is never answered twice).""",
    """/// Saves `offset`, retrying twice. A save that still fails is logged and the
/// poll goes on: an unwritable state directory must not stop the hub; the
/// cost is that a restart before the next successful save repeats the batch.
async fn save_offset(store: &OffsetStore, offset: i64) {
    let mut result = store.save(offset);
    for wait in SAVE_RETRY_WAITS {
        if result.is_ok() {
            return;
        }
        tokio::time::sleep(wait).await;
        result = store.save(offset);
    }
    if let Err(error) = result {
        warn!(kind = ?error.kind(), "cannot save the getUpdates offset; a restart may repeat this batch");
    }
}

/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
///
/// Starts from the offset in `store` and saves each new offset before the
/// batch is handled: a crash in between skips those updates instead of
/// handling them twice (at most once, so a command is never answered twice).""")
rep("""                offset = next;
                if next != previous
                    && let Some(next) = next
                    && let Err(error) = store.save(next)
                {
                    warn!(kind = ?error.kind(), "cannot save the getUpdates offset; a restart may repeat this batch");
                }
                routed""", """                offset = next;
                if next != previous
                    && let Some(next) = next
                {
                    save_offset(store, next).await;
                }
                routed""")
# tests
rep("""        let (next, routed) = route_batch(Vec::new(), Some(3), CHAT, &allowlist());
        assert_eq!((next, routed.len()), (Some(3), 0));
    }
""", """        let (next, routed) = route_batch(Vec::new(), Some(3), CHAT, &allowlist());
        assert_eq!((next, routed.len()), (Some(3), 0));
    }

    #[test]
    fn next_offset_follows_the_batch_even_below_the_old_one() {
        // Telegram restarts ids at random after a week without updates.
        let batch = vec![json!({ "update_id": 40 }), json!({ "update_id": 42 })];
        let (next, _) = route_batch(batch, Some(9000), CHAT, &allowlist());
        assert_eq!(next, Some(43));
        let (next, _) = route_batch(vec![json!({ "x": 1 })], Some(9000), CHAT, &allowlist());
        assert_eq!(next, Some(9000));
    }
""")
rep("""    #[test]
    fn nonempty_batch_without_update_ids_gets_a_short_backoff() {""", """    /// Keeps returning its updates until `getUpdates` is called with an offset
    /// above them, whatever offset it was called with before: what a bot saw
    /// after a week-long pause, when Telegram restarted ids below the offset.
    struct RestartedIds(Vec<Value>, std::sync::Mutex<i64>);

    impl UpdateSource for RestartedIds {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(
            &self,
            offset: Option<i64>,
            timeout: Duration,
        ) -> Result<Vec<Value>, ApiError> {
            // Only an offset just above the update confirms it; the stale
            // 5000 does not (the observed behaviour this models).
            let mut confirmed = self.1.lock().unwrap();
            if let Some(offset) = offset.filter(|offset| *offset < 1000) {
                *confirmed = (*confirmed).max(offset);
            }
            let batch: Vec<Value> = self
                .0
                .iter()
                .filter(|update| update["update_id"].as_i64().unwrap_or_default() >= *confirmed)
                .cloned()
                .collect();
            drop(confirmed);
            if batch.is_empty() {
                tokio::time::sleep(timeout).await;
            }
            Ok(batch)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ids_restarted_below_the_saved_offset_are_handled_once() {
        let dir = crate::hub::testdir::TempDir::new("poll-restarted-ids");
        let store = OffsetStore::open(dir.path()).unwrap();
        store.save(5000).unwrap();
        let mut texts = Vec::new();
        let source = RestartedIds(vec![text_update(7, "/brief")], std::sync::Mutex::new(0));
        let allowlist = allowlist();
        let polling = poll(&source, &allowlist, &store, |routed| {
            if let Routed::Input(input) = routed {
                texts.push(input.text.unwrap_or_default());
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(180), polling).await;
        assert_eq!(texts, ["/brief"]);
        assert_eq!(store.load(), Some(8));
    }

    /// Makes `offset` a non-empty directory, so every save fails.
    fn block_saves(dir: &std::path::Path) -> std::path::PathBuf {
        let blocker = dir.join("offset");
        std::fs::create_dir_all(blocker.join("inside")).unwrap();
        blocker
    }

    #[tokio::test(start_paused = true)]
    async fn failing_offset_saves_do_not_stop_polling() {
        let dir = crate::hub::testdir::TempDir::new("poll-save-fails");
        let store = OffsetStore::open(dir.path()).unwrap();
        block_saves(dir.path());
        let updates = vec![text_update(5, "/brief"), text_update(6, "/full")];
        // Both handled once: the in-memory offset still moves on.
        assert_eq!(handled(updates, &store).await, ["/brief", "/full"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_briefly_failing_offset_save_is_retried() {
        let dir = crate::hub::testdir::TempDir::new("poll-save-retry");
        let store = OffsetStore::open(dir.path()).unwrap();
        let blocker = block_saves(dir.path());
        // Unblocked after the first attempt, before the retry 100 ms later.
        let unblock = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::remove_dir_all(blocker).unwrap();
        });
        assert_eq!(
            handled(vec![text_update(5, "/brief")], &store).await,
            ["/brief"]
        );
        unblock.await.unwrap();
        assert_eq!(store.load(), Some(6));
    }

    #[test]
    fn nonempty_batch_without_update_ids_gets_a_short_backoff() {""")
save('updates.rs')

# ---------------- offset.rs ----------------
load('offset.rs')
rep("""use std::io::{self, Write};
use std::path::{Path, PathBuf};
""", """use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
""")
rep("""const TEMP_NAME: &str = "offset.tmp";
""", """const TEMP_NAME: &str = "offset.tmp";
/// Telegram keeps an update at most 24 hours, so an offset saved earlier
/// guards nothing. It can hurt: after a week without updates Telegram restarts
/// ids at random, possibly below it.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
""")
rep("""    /// The saved offset. A missing file is `None`; an unreadable or garbled one
    /// is logged and also `None`, so the hub still starts.
    pub fn load(&self) -> Option<i64> {
        match std::fs::read_to_string(self.dir.join(FILE_NAME)) {""", """    /// The saved offset. A missing file is `None`; an unreadable or garbled one
    /// is logged and also `None`, so the hub still starts. So is one saved
    /// more than `MAX_AGE` ago.
    pub fn load(&self) -> Option<i64> {
        let path = self.dir.join(FILE_NAME);
        let age = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        if age.is_some_and(|age| age > MAX_AGE) {
            warn!("saved getUpdates offset is older than a day; starting without it");
            return None;
        }
        match std::fs::read_to_string(&path) {""")
rep("""        store.save(8).unwrap();
        assert_eq!(store.load(), Some(8));
    }""", """        store.save(8).unwrap();
        assert_eq!(store.load(), Some(8));
    }

    #[test]
    fn an_offset_older_than_a_day_is_ignored() {
        let dir = TempDir::new("offset-stale");
        let store = OffsetStore::open(dir.path()).unwrap();
        store.save(9).unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(dir.path().join(FILE_NAME))
            .unwrap();
        file.set_modified(SystemTime::now() - MAX_AGE + Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.load(), Some(9));
        file.set_modified(SystemTime::now() - MAX_AGE - Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.load(), None);
    }""")
save('offset.rs')

# ---------------- mod.rs ----------------
load('mod.rs')
rep("""use updates::Routed;
""", """use updates::{Inbound, Routed};
""")
rep("""pub async fn run(env_file: Option<&Path>) -> anyhow::Result<()> {""", """/// The poll callback: commands go to the command worker's queue, nothing here
/// waits, so a slow command never holds up polling.
fn route_inbound(commands: &mpsc::UnboundedSender<Inbound>) -> impl FnMut(Routed) + '_ {
    move |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
        Routed::Callback(_) => info!("inbound button press"),
        Routed::Service(_) | Routed::Ignored(_) => {}
    }
}

pub async fn run(env_file: Option<&Path>) -> anyhow::Result<()> {""")
rep("""    updates::poll(
        api.as_ref(),
        &config.allowlist,
        &offsets,
        |routed| match routed {
            Routed::Input(input) if commands::is_command(&input) => {
                if commands_tx.send(input).is_err() {
                    warn!("command worker stopped; command dropped");
                }
            }
            Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
            Routed::Callback(_) => info!("inbound button press"),
            Routed::Service(_) | Routed::Ignored(_) => {}
        },
    )
    .await;""", """    updates::poll(
        api.as_ref(),
        &config.allowlist,
        &offsets,
        route_inbound(&commands_tx),
    )
    .await;""")
rep("""#[cfg(test)]
mod tests {
    use super::*;
""", """#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::*;
    use api::{ApiError, Message};
    use scheduler::{Delivery, Op, Outcome, Transport};
    use sessions::{LocateError, Located, TranscriptLocator};
    use testdir::TempDir;
    use updates::UpdateSource;

    const CHAT: i64 = -1000000000001;
    const ALLOWED: i64 = 1001;
    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";

    /// One command per call, then an idle long poll; counts calls.
    struct Batches {
        calls: AtomicUsize,
        polled_twice: Notify,
    }

    impl UpdateSource for Batches {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Value>, ApiError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let text = match call {
                0 => "/brief",
                1 => "/full",
                _ => {
                    self.polled_twice.notify_one();
                    return std::future::pending().await;
                }
            };
            Ok(vec![json!({ "update_id": call + 1, "message": {
                "message_id": call + 10, "date": 1, "text": text,
                "from": { "id": ALLOWED, "is_bot": false, "first_name": "x" },
                "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
            }})])
        }
    }

    /// Blocks each `locate` until the test lets it through.
    struct Gated {
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
        file: std::path::PathBuf,
    }

    impl TranscriptLocator for Gated {
        fn locate(&self, _: Option<i64>, _: Option<&str>) -> Result<Located, LocateError> {
            let _ = self.gate.lock().unwrap().recv();
            Ok(Located {
                session_id: SESSION.to_owned(),
                project: "C--proj".to_owned(),
                path: self.file.clone(),
            })
        }
    }

    #[derive(Default)]
    struct Sent(Mutex<Vec<String>>);

    impl Transport for Sent {
        async fn execute(&self, op: &Op) -> Delivery {
            if let Op::Send { text, .. } = op {
                self.0.lock().unwrap().push(text.clone());
            }
            Ok(Outcome::Sent(Message::default()))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_command_does_not_hold_up_polling() {
        let dir = TempDir::new("hub-slow-command");
        let file = dir.path().join(format!("{SESSION}.jsonl"));
        std::fs::write(
            &file,
            "{\\"type\\":\\"user\\",\\"message\\":{\\"role\\":\\"user\\",\\"content\\":\\"hello\\"}}\\n",
        )
        .unwrap();
        let (open_gate, gate) = std::sync::mpsc::channel();
        let sent = Arc::new(Sent::default());
        let (scheduler, outbox) = Scheduler::new(sent.clone(), BucketConfig::default());
        tokio::spawn(scheduler.run());
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let locator = Arc::new(Gated {
            gate: Mutex::new(gate),
            file: file.clone(),
        });
        tokio::spawn(commands::serve(commands_rx, outbox, locator, None));

        let source = Arc::new(Batches {
            calls: AtomicUsize::new(0),
            polled_twice: Notify::new(),
        });
        let store = Arc::new(OffsetStore::open(dir.path()).unwrap());
        let polling = {
            let (source, store) = (source.clone(), store.clone());
            tokio::spawn(async move {
                let allowlist: config::Allowlist = [ALLOWED].into_iter().collect();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx),
                )
                .await;
            })
        };

        // The first command is stuck in `locate`, yet both batches were
        // fetched and the offset saved past them.
        tokio::time::timeout(Duration::from_secs(10), source.polled_twice.notified())
            .await
            .expect("poll kept going while a command was stuck");
        assert_eq!(store.load(), Some(3));
        assert!(sent.0.lock().unwrap().is_empty());

        open_gate.send(()).unwrap();
        open_gate.send(()).unwrap();
        let answered = async {
            while sent.0.lock().unwrap().len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), answered)
            .await
            .expect("both commands answered");
        let turns = transcript::parse(&std::fs::read_to_string(&file).unwrap());
        let want = [transcript::render_brief(&turns), transcript::render_full(&turns)];
        assert_eq!(*sent.0.lock().unwrap(), want);
        polling.abort();
    }
""")
save('mod.rs')
print('ok')

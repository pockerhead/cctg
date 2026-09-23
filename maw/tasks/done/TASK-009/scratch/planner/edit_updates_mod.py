import io, os
os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'src', 'hub'))
s = ''
def rep(o, n):
    global s
    assert s.count(o) == 1, o
    s = s.replace(o, n)

p = 'updates.rs'
s = io.open(p, encoding='utf-8', newline='').read()
rep('''use std::time::Duration;

use serde_json::Value;''', '''use std::future::Future;
use std::time::Duration;

use serde_json::Value;''')
rep('''use super::config::Allowlist;
''', '''use super::config::Allowlist;
use super::offset::OffsetStore;
''')
rep('''fn stalled_batch_backoff(''', '''/// Where updates come from: `BotApi` in production, a fake in tests.
pub trait UpdateSource {
    fn chat_id(&self) -> i64;
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<Value>, ApiError>> + Send;
}

impl UpdateSource for BotApi {
    fn chat_id(&self) -> i64 {
        BotApi::chat_id(self)
    }

    async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: Duration,
    ) -> Result<Vec<Value>, ApiError> {
        BotApi::get_updates(self, offset, timeout).await
    }
}

fn stalled_batch_backoff(''')
rep('''/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
pub async fn poll(api: &BotApi, allowlist: &Allowlist, mut handle: impl FnMut(Routed)) {
    let mut offset = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        match api.get_updates(offset, POLL_TIMEOUT).await {
            Ok(raw) => {
                backoff = Duration::from_secs(1);
                let batch_len = raw.len();
                let previous = offset;
                let (next, routed) = route_batch(raw, offset, api.chat_id(), allowlist);
                offset = next;
                routed.into_iter().for_each(&mut handle);''', '''/// Long-polls forever and hands every routed update to `handle`. Errors never
/// stop the loop: 429 waits `retry_after`, other errors back off up to 30 s.
///
/// Starts from the offset in `store` and saves each new offset before the
/// batch is handled: a crash in between skips those updates instead of
/// handling them twice (at most once, so a command is never answered twice).
pub async fn poll<S: UpdateSource>(
    source: &S,
    allowlist: &Allowlist,
    store: &OffsetStore,
    mut handle: impl FnMut(Routed),
) {
    let mut offset = store.load();
    let mut backoff = Duration::from_secs(1);
    loop {
        match source.get_updates(offset, POLL_TIMEOUT).await {
            Ok(raw) => {
                backoff = Duration::from_secs(1);
                let batch_len = raw.len();
                let previous = offset;
                let (next, routed) = route_batch(raw, offset, source.chat_id(), allowlist);
                offset = next;
                if next != previous
                    && let Some(next) = next
                    && let Err(error) = store.save(next)
                {
                    warn!(kind = ?error.kind(), "cannot save the getUpdates offset; a restart may repeat this batch");
                }
                routed.into_iter().for_each(&mut handle);''')
rep('''    #[test]
    fn nonempty_batch_without_update_ids_gets_a_short_backoff() {''', '''    /// Answers like Telegram: every update at or above `offset`, or waits out
    /// the long poll when there is none. It never forgets an update, which is
    /// what Telegram does for an update that was not yet confirmed.
    struct FakeTelegram(Vec<Value>);

    impl UpdateSource for FakeTelegram {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(
            &self,
            offset: Option<i64>,
            timeout: Duration,
        ) -> Result<Vec<Value>, ApiError> {
            let batch: Vec<Value> = self
                .0
                .iter()
                .filter(|update| {
                    let id = update["update_id"].as_i64().unwrap_or_default();
                    offset.is_none_or(|offset| id >= offset)
                })
                .cloned()
                .collect();
            if batch.is_empty() {
                tokio::time::sleep(timeout).await;
            }
            Ok(batch)
        }
    }

    fn text_update(id: i64, text: &str) -> Value {
        json!({ "update_id": id, "message": message(ALLOWED, json!({ "text": text })) })
    }

    /// Polls `updates` for a few simulated minutes; returns the handled texts.
    async fn handled(updates: Vec<Value>, store: &OffsetStore) -> Vec<String> {
        let mut texts = Vec::new();
        let source = FakeTelegram(updates);
        let allowlist = allowlist();
        let polling = poll(&source, &allowlist, store, |routed| {
            if let Routed::Input(input) = routed {
                texts.push(input.text.unwrap_or_default());
            }
        });
        let _ = tokio::time::timeout(Duration::from_secs(180), polling).await;
        texts
    }

    #[tokio::test(start_paused = true)]
    async fn saved_offset_prevents_handling_an_update_twice_after_restart() {
        let dir = crate::hub::testdir::TempDir::new("poll-restart");
        let first_run = OffsetStore::open(dir.path()).unwrap();
        assert_eq!(
            handled(vec![text_update(5, "/brief")], &first_run).await,
            ["/brief"]
        );

        // Restart: a new store over the same directory, and Telegram still
        // holds update 5 because no later getUpdates confirmed it.
        let restarted = OffsetStore::open(dir.path()).unwrap();
        assert_eq!(restarted.load(), Some(6));
        let pending = vec![text_update(5, "/brief"), text_update(6, "/full")];
        assert_eq!(handled(pending.clone(), &restarted).await, ["/full"]);

        // Control: without the saved offset the old command runs again.
        let empty = crate::hub::testdir::TempDir::new("poll-no-offset");
        let fresh = OffsetStore::open(empty.path()).unwrap();
        assert_eq!(handled(pending, &fresh).await, ["/brief", "/full"]);
    }

    #[test]
    fn nonempty_batch_without_update_ids_gets_a_short_backoff() {''')
io.open(p, 'w', encoding='utf-8', newline='').write(s)

p = 'mod.rs'
s = io.open(p, encoding='utf-8', newline='').read()
rep('''pub mod api;
pub mod config;
pub mod scheduler;
pub mod updates;
''', '''pub mod api;
pub mod commands;
pub mod config;
pub mod offset;
pub mod scheduler;
pub mod sessions;
#[cfg(test)]
pub(crate) mod testdir;
pub mod updates;
''')
rep('''use anyhow::Context;
use tracing::{info, warn};
''', '''use anyhow::Context;
use tokio::sync::mpsc;
use tracing::{info, warn};
''')
rep('''use config::Config;
use scheduler::{BucketConfig, Scheduler};
''', '''use config::{Config, PROJECTS_VAR, STATE_VAR};
use offset::OffsetStore;
use scheduler::{BucketConfig, Scheduler};
use sessions::ProjectsDir;
''')
rep('''    let config = Config::load(env_file)?;
''', '''    let config = Config::load(env_file)?;
    let projects_dir = config.projects_dir.clone().with_context(|| {
        format!("no home directory found; set {PROJECTS_VAR} to the Claude Code projects directory")
    })?;
    let offsets = OffsetStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
''')
rep('''    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());

    updates::poll(&api, &config.allowlist, |routed| {
        // Handlers arrive with TASK-009/011; the outbox is kept alive for them.
        let _ = &outbox;
        match routed {
            Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
            Routed::Callback(_) => info!("inbound button press"),
            Routed::Service(_) | Routed::Ignored(_) => {}
        }
    })
    .await;''', '''    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox,
        Arc::new(ProjectsDir::new(projects_dir)),
        me.username.clone(),
    ));

    updates::poll(api.as_ref(), &config.allowlist, &offsets, |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands_tx.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
        Routed::Callback(_) => info!("inbound button press"),
        Routed::Service(_) | Routed::Ignored(_) => {}
    })
    .await;''')
io.open(p, 'w', encoding='utf-8', newline='').write(s)
print("ok")

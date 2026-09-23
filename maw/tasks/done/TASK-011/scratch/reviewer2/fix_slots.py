# Reviewer-2 changes to slots.rs (run once, after the repro tests were added).
import os
HERE = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(HERE, 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, old[:100]
    s = s.replace(old, new, 1)


rep('''//! registry and turns [`Registry::topic_work`] into scheduler jobs. It never
//! waits for Telegram: every job's answer comes back as a [`Done`] message on
//! its own channel, so ingress keeps draining while the scheduler honours a
//! `retry_after`. Saving goes to a separate task that writes the latest
//! snapshot.''', '''//! registry and turns [`Registry::topic_work`] into scheduler jobs. It never
//! awaits anything but its inputs: jobs go to a dispatch task over an
//! unbounded channel (that task waits for room in the scheduler queue), and
//! every answer comes back as a [`Done`] message, so ingress keeps draining
//! while Telegram is slow or the scheduler honours a `retry_after`. The
//! registry keeps at most one topic call per slot in flight, so the dispatch
//! queue holds at most one topic job per slot plus service-message deletes.
//! Saving goes to a separate task that writes the latest snapshot.''')
rep('''use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;''', '''use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;''')
rep('''/// Only the head of a transcript is read for its ai-title; the first one
/// appears after the first exchange (seen at up to ~0.7 MB).
const TITLE_SCAN_BYTES: u64 = 4 * 1024 * 1024;''', '''/// A transcript is scanned line by line for its first ai-title up to this
/// many bytes (the same cap as `/brief`).
const TITLE_SCAN_BYTES: u64 = 256 * 1024 * 1024;''')
rep('''    /// The bot may delete service messages (`can_delete_messages`).
    pub can_delete: bool,
    /// How long an agent of an unknown session waits for its SessionStart
    /// hook before the session is adopted as top-level.
    pub hook_wait: Duration,''', '''    /// The bot may delete service messages (`can_delete_messages`).
    pub can_delete: bool,''')
rep('''            can_delete: true,
            hook_wait: Duration::from_secs(10),''', '''            can_delete: true,''')
rep('''struct Pending {
    conn: u64,
    host: String,
    cwd: String,
    deadline: Instant,
}
''', '''/// A job for the dispatch task.
#[derive(Debug)]
enum Work {
    Topic(TopicJob),
    Delete,
}
''')
rep('''/// First ai-title in the head of the transcript; `None` when there is none
/// yet or the file is not on this machine.
pub fn read_title(path: &str) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(TITLE_SCAN_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    transcript::ai_title(&String::from_utf8_lossy(&bytes))
}''', '''/// First ai-title of the transcript; `None` when there is none yet or the
/// file is not on this machine.
pub fn read_title(path: &str) -> Option<String> {
    first_ai_title(std::fs::File::open(path).ok()?, TITLE_SCAN_BYTES)
}

/// Streams `jsonl` line by line, at most `limit` bytes, until an ai-title.
fn first_ai_title(jsonl: impl Read, limit: u64) -> Option<String> {
    let mut reader = BufReader::new(jsonl.take(limit));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\\n', &mut line).ok()? == 0 {
            return None;
        }
        if let Some(title) = transcript::ai_title(&String::from_utf8_lossy(&line)) {
            return Some(title);
        }
    }
}''')
rep('''pub struct Slots {
    registry: Registry,
    outbox: Outbox,
    options: Options,''', '''pub struct Slots {
    registry: Registry,
    dispatch: mpsc::UnboundedSender<(Work, Op)>,
    options: Options,''')
rep('''    conns: HashMap<u64, (String, mpsc::Sender<HubMsg>)>,
    pending: HashMap<String, Pending>,''', '''    conns: HashMap<u64, (String, mpsc::Sender<HubMsg>)>,
    /// Agents of sessions no SessionStart announced yet: session -> conn.
    pending: HashMap<String, u64>,''')
rep('''    /// Starts the save task. Returns the actor and the view for `/brief`.''',
    '''    /// Starts the save and dispatch tasks. Returns the actor and the view
    /// for `/brief`.''')
rep('''        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let now = Instant::now();
        let slots = Self {
            registry,
            outbox,
            saver,''', '''        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let (dispatch, work) = mpsc::unbounded_channel();
        tokio::spawn(dispatch_loop(outbox, work, done_tx.clone()));
        let now = Instant::now();
        let slots = Self {
            registry,
            dispatch,
            saver,''')
rep('''        let mut done = self.done_rx.take().expect("run once");
        self.pump().await;
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                Some(event) = agents.recv() => self.on_agent(event),
                Some(post) = hooks.recv() => self.on_hook(&post),
                Some(control) = control.recv() => self.on_control(control).await,
                Some(finished) = done.recv() => self.on_done(finished),
                () = sleep_until(deadline) => self.on_tick(),
            }
            self.pump().await;
        }
    }

    fn next_deadline(&self) -> Instant {
        let now = Instant::now();
        let mut deadline = self.next_retry;
        if self.grace_until > now {
            deadline = deadline.min(self.grace_until);
        }
        for pending in self.pending.values() {
            deadline = deadline.min(pending.deadline);
        }
        deadline
    }''', '''        let mut done = self.done_rx.take().expect("run once");
        self.pump();
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                Some(event) = agents.recv() => self.on_agent(event),
                Some(post) = hooks.recv() => self.on_hook(&post),
                Some(control) = control.recv() => self.on_control(control),
                Some(finished) = done.recv() => self.on_done(finished),
                () = sleep_until(deadline) => self.on_tick(),
            }
            self.pump();
        }
    }

    fn next_deadline(&self) -> Instant {
        if self.grace_until > Instant::now() {
            self.next_retry.min(self.grace_until)
        } else {
            self.next_retry
        }
    }''')
rep('''                } else {
                    debug!(
                        conn,
                        session = short(&session),
                        "agent of an unknown session waits for its hook"
                    );
                    self.pending.insert(
                        session.clone(),
                        Pending {
                            conn,
                            host: register.host,
                            cwd: register.cwd,
                            deadline: Instant::now() + self.options.hook_wait,
                        },
                    );
                }''', '''                } else {
                    debug!(
                        conn,
                        session = short(&session),
                        "agent of an unknown session waits for its SessionStart"
                    );
                    self.pending.insert(session.clone(), conn);
                }''')
rep('''                    if self.pending.get(&session).is_some_and(|p| p.conn == conn) {''',
    '''                    if self.pending.get(&session) == Some(&conn) {''')
rep('''            && let Some(pending) = self.pending.remove(session)
        {
            self.registry.agent_connected(session, pending.conn);
        }''', '''            && let Some(conn) = self.pending.remove(session)
        {
            self.registry.agent_connected(session, conn);
        }''')
rep('''    async fn on_control(&mut self, control: Control) {''', '''    fn on_control(&mut self, control: Control) {''')
rep('''        let answer = self.outbox.submit(Op::Delete { message_id }).await;
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let _ = done.send(Done::Delete(answer.await.ok()));
        });
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        let expired: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(session, _)| session.clone())
            .collect();
        for session in expired {
            if let Some(pending) = self.pending.remove(&session) {
                let slot = self.registry.adopt(&session, &pending.host, &pending.cwd);
                self.registry.agent_connected(&session, pending.conn);
                info!(
                    session = short(&session),
                    ordinal = self.ordinal(slot),
                    "session known only from its agent adopted as top-level"
                );
            }
        }
        if now >= self.next_retry {''', '''        self.hand_off(Work::Delete, Op::Delete { message_id });
    }

    /// Never waits: the dispatch task does.
    fn hand_off(&self, work: Work, op: Op) {
        if self.dispatch.send((work, op)).is_err() {
            debug!("dispatch task stopped; job dropped");
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_retry {''')
rep('''        let Some(delivery) = delivery else {
            if let TopicJob::Create { slot, .. } | TopicJob::Edit { slot, .. } = job {
                self.registry.release(slot);
            }
            return;
        };''', '''        let Some(delivery) = delivery else {
            let (TopicJob::Create { slot, .. }
            | TopicJob::Edit { slot, .. }
            | TopicJob::Separator { slot, .. }) = job;
            self.registry.release(slot);
            return;
        };''')
rep('''            TopicJob::Separator {
                slot, thread_id, ..
            } => {
                if topic_gone(&delivery) {
                    warn!(
                        ordinal = self.ordinal(slot),
                        "forum topic is gone; creating a replacement"
                    );
                    self.registry.topic_invalid(slot, thread_id);
                } else if let Err(error) = delivery {
                    warn!(%error, "session separator not delivered");
                }
            }''', '''            TopicJob::Separator {
                slot,
                thread_id,
                text,
            } => {
                if delivery.is_ok() {
                    self.registry.topic_separated(slot, thread_id, &text);
                } else if topic_gone(&delivery) {
                    warn!(
                        ordinal = self.ordinal(slot),
                        "forum topic is gone; creating a replacement"
                    );
                    self.registry.topic_invalid(slot, thread_id);
                } else {
                    if let Err(error) = &delivery {
                        warn!(%error, ordinal = self.ordinal(slot), "session separator not delivered; retrying later");
                    }
                    self.registry.topic_failed(slot, icons);
                }
            }''')
rep('''    /// Hands pending topic work to the scheduler, publishes the view and
    /// the snapshot to save.
    async fn pump(&mut self) {''', '''    /// Hands pending topic work to the dispatch task, publishes the view and
    /// the snapshot to save.
    fn pump(&mut self) {''')
rep('''            let answer = self.outbox.submit(op).await;
            let done = self.done_tx.clone();
            tokio::spawn(async move {
                let delivery = answer.await.ok();
                let _ = done.send(Done::Topic { job, delivery });
            });
        }''', '''            self.hand_off(Work::Topic(job), op);
        }''')
rep('''async fn save_once(store: &RegistryStore, bytes: Arc<Vec<u8>>) -> std::io::Result<()> {''',
    '''/// Enqueues jobs in order, waiting for room in the scheduler queue so the
/// actor never has to; each answer comes back as a [`Done`].
async fn dispatch_loop(
    outbox: Outbox,
    mut work: mpsc::UnboundedReceiver<(Work, Op)>,
    done: mpsc::UnboundedSender<Done>,
) {
    while let Some((work, op)) = work.recv().await {
        let answer = outbox.submit(op).await;
        let done = done.clone();
        tokio::spawn(async move {
            let delivery = answer.await.ok();
            let _ = done.send(match work {
                Work::Topic(job) => Done::Topic { job, delivery },
                Work::Delete => Done::Delete(delivery),
            });
        });
    }
}

async fn save_once(store: &RegistryStore, bytes: Arc<Vec<u8>>) -> std::io::Result<()> {''')

# ---- tests ----
rep('''    fn options() -> Options {
        Options {
            grace: Duration::ZERO,
            hook_wait: Duration::from_millis(200),
            ..Options::default()
        }
    }''', '''    fn options() -> Options {
        Options {
            grace: Duration::ZERO,
            ..Options::default()
        }
    }''')
rep('''        // No hook at all: adopted after hook_wait.
        rig.agent(2, B).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(count(&ops, is_create), 2);
        assert!(
            matches!(&ops[1], Op::CreateTopic { name, .. } if name == "[box] Project #2 · bbbbbbbb")
        );
    }''', '''        // No hook yet: the agent waits, however long, and makes no topic.
        rig.agent(2, B).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(count(&ops, is_create), 2);
        assert!(
            matches!(&ops[1], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project #2 · bbbbbbbb" && icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
    }''')
rep('''    #[tokio::test]
    async fn the_ai_title_replaces_the_short_id() {''', '''    #[test]
    fn the_ai_title_is_found_past_the_head_of_a_long_transcript() {
        let filler = "{\\"type\\":\\"user\\",\\"message\\":{\\"role\\":\\"user\\",\\"content\\":\\"x\\"}}\\n";
        let mut jsonl = filler.repeat(5 * 1024 * 1024 / filler.len() + 1);
        assert!(jsonl.len() > 5 * 1024 * 1024);
        jsonl.push_str("{\\"type\\":\\"ai-title\\",\\"aiTitle\\":\\"Late title\\"}\\n");
        let dir = TempDir::new("slots-title");
        let path = dir.path().join("long.jsonl");
        std::fs::write(&path, &jsonl).unwrap();
        assert_eq!(
            read_title(&path.display().to_string()).as_deref(),
            Some("Late title")
        );
        // Past the cap: not read.
        assert_eq!(
            first_ai_title(jsonl.as_bytes(), (jsonl.len() - 10) as u64),
            None
        );
        assert_eq!(read_title(&dir.path().join("none.jsonl").display().to_string()), None);
    }

    #[tokio::test]
    async fn the_ai_title_replaces_the_short_id() {''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')

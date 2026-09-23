//! Slot actor: the one owner of the [`Registry`].
//!
//! It drains agent and hook ingress and forum service messages, updates the
//! registry and turns [`Registry::topic_work`] into scheduler jobs. It never
//! waits for Telegram: every job's answer comes back as a [`Done`] message on
//! its own channel, so ingress keeps draining while the scheduler honours a
//! `retry_after`. Saving goes to a separate task that writes the latest
//! snapshot.
//!
//! Logs carry short session ids, slot ordinals and fixed text; never a path,
//! a folder or a title.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};

use super::api::ApiError;
use super::ingress::AgentEvent;
use super::registry::{Icons, Registry, RegistryStore, SlotId, TopicJob, TopicView};
use super::scheduler::{Delivery, Op, Outbox, Outcome};
use crate::wire::{AgentMsg, HookPost, HubMsg};

/// Only the head of a transcript is read for its ai-title; the first one
/// appears after the first exchange (seen at up to ~0.7 MB).
const TITLE_SCAN_BYTES: u64 = 4 * 1024 * 1024;
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];
const SHORT_ID: usize = 8;

#[derive(Debug, Clone)]
pub struct Options {
    pub icons: Icons,
    /// The bot may delete service messages (`can_delete_messages`).
    pub can_delete: bool,
    /// How long an agent of an unknown session waits for its SessionStart
    /// hook before the session is adopted as top-level.
    pub hook_wait: Duration,
    /// After start, topic edits wait this long: agents are reconnecting.
    pub grace: Duration,
    /// Failed topic calls are tried again this often.
    pub retry_every: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            icons: Icons::default(),
            can_delete: true,
            hook_wait: Duration::from_secs(10),
            // Longer than the agent's 30 s maximum reconnect backoff.
            grace: Duration::from_secs(45),
            retry_every: Duration::from_secs(60),
        }
    }
}

/// From the update poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// A `forum_topic_edited` service message.
    TopicEdited {
        thread_id: Option<i64>,
        message_id: i64,
    },
}

#[derive(Debug)]
enum Done {
    Topic {
        job: TopicJob,
        /// `None`: the scheduler stopped.
        delivery: Option<Delivery>,
    },
    Delete(Option<Delivery>),
    Title {
        session: String,
        title: Option<String>,
    },
}

struct Pending {
    conn: u64,
    host: String,
    cwd: String,
    deadline: Instant,
}

fn short(session_id: &str) -> &str {
    session_id
        .char_indices()
        .nth(SHORT_ID)
        .map_or(session_id, |(end, _)| &session_id[..end])
}

fn telegram_error(delivery: &Delivery, needles: &[&str]) -> bool {
    match delivery {
        Err(ApiError::Telegram {
            code: 400,
            description,
        }) => {
            let description = description.to_ascii_lowercase();
            needles.iter().any(|needle| description.contains(needle))
        }
        _ => false,
    }
}

/// The topic was deleted in Telegram.
fn topic_gone(delivery: &Delivery) -> bool {
    telegram_error(
        delivery,
        &[
            "topic_id_invalid",
            "topic_deleted",
            "message thread not found",
        ],
    )
}

/// `editForumTopic` with the name and icon it already has.
fn not_modified(delivery: &Delivery) -> bool {
    telegram_error(delivery, &["topic_not_modified"])
}

/// First ai-title in the head of the transcript; `None` when there is none
/// yet or the file is not on this machine.
pub fn read_title(path: &str) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(TITLE_SCAN_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    transcript::ai_title(&String::from_utf8_lossy(&bytes))
}

pub struct Slots {
    registry: Registry,
    outbox: Outbox,
    options: Options,
    saver: watch::Sender<Option<Arc<Vec<u8>>>>,
    view: watch::Sender<Arc<TopicView>>,
    conns: HashMap<u64, (String, mpsc::Sender<HubMsg>)>,
    pending: HashMap<String, Pending>,
    reading: HashSet<String>,
    delete_warned: bool,
    grace_until: Instant,
    next_retry: Instant,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: Option<mpsc::UnboundedReceiver<Done>>,
}

impl Slots {
    /// Starts the save task. Returns the actor and the view for `/brief`.
    pub fn new(
        registry: Registry,
        store: RegistryStore,
        outbox: Outbox,
        options: Options,
    ) -> (Self, watch::Receiver<Arc<TopicView>>) {
        let (saver, saves) = watch::channel(None);
        tokio::spawn(save_loop(store, saves));
        let (view, view_rx) = watch::channel(Arc::new(registry.topic_view()));
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let now = Instant::now();
        let slots = Self {
            registry,
            outbox,
            saver,
            view,
            conns: HashMap::new(),
            pending: HashMap::new(),
            reading: HashSet::new(),
            delete_warned: false,
            grace_until: now + options.grace,
            next_retry: now + options.retry_every,
            done_tx,
            done_rx: Some(done_rx),
            options,
        };
        (slots, view_rx)
    }

    /// Runs for the life of the hub; a closed input channel is just no
    /// longer polled.
    pub async fn run(
        mut self,
        mut agents: mpsc::Receiver<AgentEvent>,
        mut hooks: mpsc::Receiver<HookPost>,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) {
        let mut done = self.done_rx.take().expect("run once");
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
    }

    fn on_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Registered {
                conn,
                register,
                to_agent,
            } => {
                let session = register.session_id.clone();
                if self.registry.agent_connected(&session, conn) {
                    info!(
                        conn,
                        session = short(&session),
                        "agent bound to its session"
                    );
                } else {
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
                }
                self.conns.insert(conn, (session, to_agent));
            }
            AgentEvent::Message { conn, msg } => {
                let Some((session, _)) = self.conns.get(&conn) else {
                    return;
                };
                match msg {
                    AgentMsg::PermissionRequest(_) => self.registry.set_waiting(session, true),
                    _ => debug!(conn, "agent message not routed yet"),
                }
            }
            AgentEvent::Disconnected { conn } => {
                if let Some((session, _)) = self.conns.remove(&conn) {
                    self.registry.agent_disconnected(&session, conn);
                    if self.pending.get(&session).is_some_and(|p| p.conn == conn) {
                        self.pending.remove(&session);
                    }
                }
            }
        }
    }

    fn on_hook(&mut self, post: &HookPost) {
        let followup = self.registry.apply_hook(post);
        let session = post.session_id.as_str();
        if self.registry.sessions.contains_key(session)
            && let Some(pending) = self.pending.remove(session)
        {
            self.registry.agent_connected(session, pending.conn);
        }
        if let Some((session, path)) = followup.read_title {
            self.read_title(session, path);
        }
    }

    fn read_title(&mut self, session: String, path: String) {
        if !self.reading.insert(session.clone()) {
            return;
        }
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let title = tokio::task::spawn_blocking(move || read_title(&path))
                .await
                .unwrap_or_default();
            let _ = done.send(Done::Title { session, title });
        });
    }

    async fn on_control(&mut self, control: Control) {
        let Control::TopicEdited {
            thread_id,
            message_id,
        } = control;
        if !self.options.can_delete
            || thread_id
                .and_then(|t| self.registry.slot_by_topic(t))
                .is_none()
        {
            return;
        }
        let answer = self.outbox.submit(Op::Delete { message_id }).await;
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
        if now >= self.next_retry {
            self.registry.retry_failed();
            self.next_retry = now + self.options.retry_every;
        }
    }

    fn ordinal(&self, slot: SlotId) -> u32 {
        self.registry.slot(slot).map_or(0, |slot| slot.ordinal)
    }

    fn on_done(&mut self, done: Done) {
        match done {
            Done::Topic { job, delivery } => self.on_topic_done(job, delivery),
            Done::Delete(delivery) => match delivery {
                Some(Ok(_)) | None => {}
                Some(Err(error)) if !self.delete_warned => {
                    self.delete_warned = true;
                    warn!(%error, "cannot delete a forum service message; later failures are not logged");
                }
                Some(Err(error)) => debug!(%error, "forum service message not deleted"),
            },
            Done::Title { session, title } => {
                self.reading.remove(&session);
                if let Some(title) = title {
                    self.registry.set_title(&session, &title);
                }
            }
        }
    }

    fn on_topic_done(&mut self, job: TopicJob, delivery: Option<Delivery>) {
        let icons = &self.options.icons;
        let Some(delivery) = delivery else {
            if let TopicJob::Create { slot, .. } | TopicJob::Edit { slot, .. } = job {
                self.registry.release(slot);
            }
            return;
        };
        match job {
            TopicJob::Create { slot, name, icon } => match delivery {
                Ok(Outcome::Topic(topic)) if topic.message_thread_id != 0 => {
                    info!(ordinal = self.ordinal(slot), "forum topic created");
                    self.registry.topic_created(
                        slot,
                        topic.message_thread_id,
                        &name,
                        icon.as_deref(),
                    );
                }
                Ok(_) => {
                    warn!("createForumTopic answered without a topic id");
                    self.registry.topic_failed(slot, icons);
                }
                Err(error) => {
                    warn!(%error, ordinal = self.ordinal(slot), "cannot create a forum topic; retrying later");
                    self.registry.topic_failed(slot, icons);
                }
            },
            TopicJob::Edit {
                slot,
                thread_id,
                name,
                icon,
            } => {
                if delivery.is_ok() || not_modified(&delivery) {
                    self.registry
                        .topic_edited(slot, thread_id, name.as_deref(), icon.as_deref());
                } else if topic_gone(&delivery) {
                    warn!(
                        ordinal = self.ordinal(slot),
                        "forum topic is gone; creating a replacement"
                    );
                    self.registry.topic_invalid(slot, thread_id);
                } else {
                    if let Err(error) = &delivery {
                        warn!(%error, ordinal = self.ordinal(slot), "cannot edit a forum topic; retrying later");
                    }
                    self.registry.topic_failed(slot, icons);
                }
            }
            TopicJob::Separator {
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
            }
        }
    }

    /// Hands pending topic work to the scheduler, publishes the view and
    /// the snapshot to save.
    async fn pump(&mut self) {
        let edits = Instant::now() >= self.grace_until;
        for job in self.registry.topic_work(&self.options.icons, edits) {
            let op = match &job {
                TopicJob::Create { name, icon, .. } => Op::CreateTopic {
                    name: name.clone(),
                    icon_custom_emoji_id: icon.clone(),
                },
                TopicJob::Edit {
                    thread_id,
                    name,
                    icon,
                    ..
                } => Op::EditTopic {
                    thread_id: *thread_id,
                    name: name.clone(),
                    icon_custom_emoji_id: icon.clone(),
                },
                TopicJob::Separator {
                    thread_id, text, ..
                } => Op::Send {
                    thread_id: Some(*thread_id),
                    text: text.clone(),
                    reply_markup: None,
                    permission: false,
                },
            };
            let answer = self.outbox.submit(op).await;
            let done = self.done_tx.clone();
            tokio::spawn(async move {
                let delivery = answer.await.ok();
                let _ = done.send(Done::Topic { job, delivery });
            });
        }
        let view = self.registry.topic_view();
        self.view.send_if_modified(|current| {
            let changed = **current != view;
            if changed {
                *current = Arc::new(view);
            }
            changed
        });
        if self.registry.dirty {
            self.registry.dirty = false;
            self.saver
                .send_replace(Some(Arc::new(RegistryStore::encode(&self.registry))));
        }
    }
}

async fn save_once(store: &RegistryStore, bytes: Arc<Vec<u8>>) -> std::io::Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save(&bytes))
        .await
        .map_err(|_| std::io::Error::other("registry save worker failed"))?
}

/// Writes the latest snapshot; snapshots that arrive meanwhile collapse into one.
async fn save_loop(store: RegistryStore, mut saves: watch::Receiver<Option<Arc<Vec<u8>>>>) {
    while saves.changed().await.is_ok() {
        let Some(bytes) = saves.borrow_and_update().clone() else {
            continue;
        };
        let mut result = save_once(&store, bytes.clone()).await;
        for wait in SAVE_RETRY_WAITS {
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(wait).await;
            result = save_once(&store, bytes.clone()).await;
        }
        if let Err(error) = result {
            warn!(kind = ?error.kind(), "cannot save the slot registry; the next change tries again");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::hub::api::{ForumTopic, Message};
    use crate::hub::registry::{ICON_ALIVE, ICON_DEAD, ICON_NO_CHANNEL};
    use crate::hub::scheduler::{BucketConfig, Scheduler, Transport};
    use crate::hub::testdir::TempDir;
    use crate::wire::{HookEvent, PermissionRequest, Register};

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const CWD: &str = r"C:\Work\Project";
    const WAIT: Duration = Duration::from_secs(10);

    /// Records every op. Topics are numbered from 100. `edit_errors` answers
    /// the next edits with these Telegram errors (last first).
    #[derive(Default)]
    struct Fake {
        ops: Mutex<Vec<Op>>,
        next_topic: Mutex<i64>,
        edit_errors: Mutex<Vec<&'static str>>,
        delete_error: Option<&'static str>,
    }

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            self.ops.lock().unwrap().push(op.clone());
            let error = |description: &str| {
                Err(ApiError::Telegram {
                    code: 400,
                    description: description.to_owned(),
                })
            };
            match op {
                Op::CreateTopic { name, .. } => {
                    let mut next = self.next_topic.lock().unwrap();
                    *next = (*next).max(100);
                    let topic = ForumTopic {
                        message_thread_id: *next,
                        name: name.clone(),
                        icon_custom_emoji_id: None,
                    };
                    *next += 1;
                    Ok(Outcome::Topic(topic))
                }
                Op::EditTopic { .. } => match self.edit_errors.lock().unwrap().pop() {
                    Some(description) => error(description),
                    None => Ok(Outcome::Done),
                },
                Op::Delete { .. } => match self.delete_error {
                    Some(description) => error(description),
                    None => Ok(Outcome::Done),
                },
                _ => Ok(Outcome::Sent(Message::default())),
            }
        }
    }

    impl Fake {
        fn ops(&self) -> Vec<Op> {
            self.ops.lock().unwrap().clone()
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        agents: mpsc::Sender<AgentEvent>,
        hooks: mpsc::Sender<HookPost>,
        control: mpsc::UnboundedSender<Control>,
        view: watch::Receiver<Arc<TopicView>>,
        dir: TempDir,
        _to_agent: Vec<mpsc::Receiver<HubMsg>>,
    }

    fn options() -> Options {
        Options {
            grace: Duration::ZERO,
            hook_wait: Duration::from_millis(200),
            ..Options::default()
        }
    }

    fn rig(fake: Fake, options: Options) -> Rig {
        let dir = TempDir::new("slots");
        rig_in(fake, options, dir)
    }

    fn rig_in(fake: Fake, options: Options, dir: TempDir) -> Rig {
        let fake = Arc::new(fake);
        let store = RegistryStore::open(dir.path()).unwrap();
        let registry = store.load().unwrap();
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        tokio::spawn(scheduler.run());
        let (slots, view) = Slots::new(registry, store, outbox, options);
        let (agents, agents_rx) = mpsc::channel(16);
        let (hooks, hooks_rx) = mpsc::channel(16);
        let (control, control_rx) = mpsc::unbounded_channel();
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
        Rig {
            fake,
            agents,
            hooks,
            control,
            view,
            dir,
            _to_agent: Vec::new(),
        }
    }

    fn hook(session: &str, event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            CWD.into(),
            String::new(),
            event,
        )
    }

    fn start(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    impl Rig {
        async fn hook(&self, post: HookPost) {
            self.hooks.send(post).await.unwrap();
        }

        async fn agent(&mut self, conn: u64, session: &str) {
            let (to_agent, rx) = mpsc::channel(4);
            self._to_agent.push(rx);
            let register = Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
            };
            self.agents
                .send(AgentEvent::Registered {
                    conn,
                    register,
                    to_agent,
                })
                .await
                .unwrap();
        }

        /// Waits until `count` ops were made, then a little more to catch extras.
        async fn ops_after(&self, count: usize) -> Vec<Op> {
            let reached = async {
                while self.fake.ops().len() < count {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            };
            tokio::time::timeout(WAIT, reached)
                .await
                .expect("ops in time");
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.fake.ops()
        }
    }

    fn icon_edit(op: &Op) -> Option<&str> {
        match op {
            Op::EditTopic {
                icon_custom_emoji_id: Some(icon),
                ..
            } => Some(icon),
            _ => None,
        }
    }

    fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {
        ops.iter().filter(|op| pred(op)).count()
    }

    fn is_create(op: &Op) -> bool {
        matches!(op, Op::CreateTopic { .. })
    }

    #[tokio::test]
    async fn one_slot_lives_through_hook_agent_end_and_the_next_session() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project · aaaaaaaa" && icon_custom_emoji_id.as_deref() == Some(ICON_NO_CHANNEL))
        );

        rig.agent(1, A).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert_eq!(icon_edit(&ops[1]), Some(ICON_ALIVE));

        // The edit's service message is deleted; one in an unknown topic is not.
        for (thread, message_id) in [(Some(100), 55), (Some(999), 56), (None, 57)] {
            rig.control
                .send(Control::TopicEdited {
                    thread_id: thread,
                    message_id,
                })
                .unwrap();
        }
        let ops = rig.ops_after(3).await;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(matches!(ops[2], Op::Delete { message_id: 55 }));

        rig.hook(hook(A, HookEvent::SessionEnd { reason: None }))
            .await;
        let ops = rig.ops_after(4).await;
        assert_eq!(ops.len(), 4, "{ops:?}");
        assert_eq!(icon_edit(&ops[3]), Some(ICON_DEAD));
        assert!(matches!(
            &ops[3],
            Op::EditTopic {
                thread_id: 100,
                name: None,
                ..
            }
        ));

        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(6).await;
        assert_eq!(count(&ops, is_create), 1, "no second topic: {ops:?}");
        let separators: Vec<&Op> = ops
            .iter()
            .filter(|op| matches!(op, Op::Send { .. }))
            .collect();
        assert_eq!(separators.len(), 1);
        assert!(
            matches!(separators[0], Op::Send { thread_id: Some(100), text, .. }
            if text == "── session bbbbbbbb · new ──")
        );
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), .. }
            if name == "[box] Project · bbbbbbbb")
        );

        // Saved to disk and routed for /brief.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let saved = RegistryStore::open(rig.dir.path()).unwrap().load().unwrap();
        assert_eq!(saved.slots.len(), 1);
        assert_eq!(saved.slots[0].current_session.as_deref(), Some(B));
        assert_eq!(saved.slots[0].topic_id, Some(100));
        assert_eq!(
            rig.view.borrow().get(&100).map(|(s, _)| s.as_str()),
            Some(B)
        );
    }

    #[tokio::test]
    async fn two_live_sessions_make_two_topics_and_waiting_shows() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(2).await;
        let names: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                Op::CreateTopic { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["[box] Project · aaaaaaaa", "[box] Project #2 · bbbbbbbb"]
        );
        rig.agent(1, B).await;
        rig.agents
            .send(AgentEvent::Message {
                conn: 1,
                msg: AgentMsg::PermissionRequest(PermissionRequest {
                    request_id: "abcde".into(),
                    tool_name: "Bash".into(),
                    description: "d".into(),
                    input_preview: "p".into(),
                }),
            })
            .await
            .unwrap();
        let ops = rig.ops_after(3).await;
        let last = icon_edit(ops.last().unwrap());
        // Either the alive edit coalesced into waiting or both happened.
        assert_eq!(last, Some(crate::hub::registry::ICON_WAITING), "{ops:?}");
    }

    #[tokio::test]
    async fn an_agent_before_its_hook_is_bound_without_a_second_topic() {
        let mut rig = rig(Fake::default(), options());
        rig.agent(1, A).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rig.fake.ops().is_empty(), "waits for the hook");
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { icon_custom_emoji_id, .. }
            if icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );

        // No hook at all: adopted after hook_wait.
        rig.agent(2, B).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(count(&ops, is_create), 2);
        assert!(
            matches!(&ops[1], Op::CreateTopic { name, .. } if name == "[box] Project #2 · bbbbbbbb")
        );
    }

    #[tokio::test]
    async fn nested_and_subagent_events_make_no_topics() {
        let rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SubagentStart {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SubagentStop {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
                agent_transcript_path: None,
                last_assistant_message: None,
            },
        ))
        .await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1, "{ops:?}");
    }

    #[tokio::test]
    async fn a_deleted_topic_is_replaced_once() {
        let fake = Fake {
            edit_errors: Mutex::new(vec!["Bad Request: TOPIC_ID_INVALID"]),
            ..Fake::default()
        };
        let mut rig = rig(fake, options());
        rig.hook(start(A, 10)).await;
        rig.hook(start(B, 11)).await;
        rig.ops_after(2).await;
        rig.agent(1, A).await; // edit of topic 100 fails: gone
        let ops = rig.ops_after(4).await;
        assert_eq!(ops.len(), 4, "{ops:?}");
        assert!(
            matches!(&ops[3], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project · aaaaaaaa" && icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
        // Topic 101 of the other slot was never touched.
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::EditTopic { thread_id: 101, .. }))
        );
    }

    #[tokio::test]
    async fn topic_not_modified_counts_as_applied() {
        let fake = Fake {
            edit_errors: Mutex::new(vec!["Bad Request: TOPIC_NOT_MODIFIED"]),
            ..Fake::default()
        };
        // A short retry period: a failure would be retried within the test.
        let mut rig = rig(
            fake,
            Options {
                retry_every: Duration::from_millis(100),
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent(1, A).await;
        rig.ops_after(2).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let ops = rig.fake.ops();
        assert_eq!(ops.len(), 2, "applied: no retry, no replacement: {ops:?}");
    }

    #[tokio::test]
    async fn without_the_delete_right_nothing_is_deleted() {
        let rig = rig(
            Fake::default(),
            Options {
                can_delete: false,
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.control
            .send(Control::TopicEdited {
                thread_id: Some(100),
                message_id: 5,
            })
            .unwrap();
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
    }

    #[tokio::test]
    async fn a_restart_keeps_slots_and_waits_out_the_grace() {
        // What the previous run saved: a slot whose session was alive.
        let dir = TempDir::new("slots-restart");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, 10));
        registry.agent_connected(A, 1);
        for job in registry.topic_work(&Icons::default(), true) {
            if let TopicJob::Create { slot, name, icon } = job {
                registry.topic_created(slot, 100, &name, icon.as_deref());
            }
        }
        store.save(&RegistryStore::encode(&registry)).unwrap();

        let mut rig = rig_in(
            Fake::default(),
            Options {
                grace: Duration::from_millis(400),
                ..options()
            },
            dir,
        );
        // The agent reconnects within the grace: no edit at all.
        rig.agent(7, A).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(rig.fake.ops().is_empty(), "{:?}", rig.fake.ops());
        // A new concurrent session still gets its topic.
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(count(&ops, is_create), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { name, .. } if name == "[box] Project #2 · bbbbbbbb")
        );
    }

    #[tokio::test]
    async fn the_ai_title_replaces_the_short_id() {
        let rig = rig(Fake::default(), options());
        let transcript = rig.dir.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n{\"type\":\"ai-title\",\"aiTitle\":\"Slot registry\"}\n",
        )
        .unwrap();
        let mut first = start(A, 10);
        first.transcript_path = transcript.display().to_string();
        rig.hook(first).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ))
        .await;
        let ops = rig.ops_after(2).await;
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), icon_custom_emoji_id: None, .. }
            if name == "[box] Project · Slot registry"),
            "{ops:?}"
        );
    }
}

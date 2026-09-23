//! Slot actor: the one owner of the [`Registry`].
//!
//! It drains agent and hook ingress and forum service messages, updates the
//! registry and turns [`Registry::topic_work`] into scheduler jobs. It never
//! awaits anything but its inputs: jobs go to a dispatch task over an
//! unbounded channel (that task waits for room in the scheduler queue), and
//! every answer comes back as a [`Done`] message, so ingress keeps draining
//! while Telegram is slow or the scheduler honours a `retry_after`. The
//! registry keeps at most one topic call per slot in flight, so the dispatch
//! queue holds at most one topic job per slot plus service-message deletes.
//! Saving goes to a separate task that writes the latest snapshot.
//!
//! Topic messages from allowlisted users go to the agent of the slot's
//! current session with a non-blocking `try_send`; agent replies go back to
//! the session's topic through the same dispatch task. At most
//! [`MAX_QUEUED_MESSAGES`] such messages wait for Telegram at a time.
//!
//! Logs carry short session ids, slot ordinals and fixed text; never a path,
//! a folder, a title or message text.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};
use transcript::{SplitOptions, split_for_telegram};

use super::api::{ApiError, Document};
use super::ingress::AgentEvent;
use super::registry::{Icons, Registry, RegistryStore, SlotId, TopicJob, TopicView};
use super::scheduler::{Delivery, Op, Outbox, Outcome};
use super::updates::Inbound;
use crate::wire::{AgentMsg, HookEvent, HookPost, HubMsg};

/// A transcript is scanned line by line for its first ai-title up to this
/// many bytes (the same cap as `/brief`).
const TITLE_SCAN_BYTES: u64 = 256 * 1024 * 1024;
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];
const SHORT_ID: usize = 8;
/// Reply chunks and notices waiting for Telegram; beyond this a new reply or
/// notice is dropped whole (the group allows ~20 messages a minute anyway).
pub const MAX_QUEUED_MESSAGES: usize = 256;
pub const OFFLINE_NOTICE: &str = "Сессия этой темы не на связи, сообщение не доставлено.";
pub const TEXT_ONLY_NOTICE: &str = "В сессию пока доходят только текстовые сообщения.";

#[derive(Debug, Clone)]
pub struct Options {
    pub icons: Icons,
    /// The forum supergroup, passed to Claude as `chat_id` meta.
    pub chat_id: i64,
    /// The bot may delete service messages (`can_delete_messages`).
    pub can_delete: bool,
    /// After start, topic edits wait this long: agents are reconnecting.
    pub grace: Duration,
    /// Failed topic calls are tried again this often.
    pub retry_every: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            icons: Icons::default(),
            chat_id: 0,
            can_delete: true,
            // Longer than the agent's 30 s maximum reconnect backoff.
            grace: Duration::from_secs(45),
            retry_every: Duration::from_secs(60),
        }
    }
}

/// From the update poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// A `forum_topic_edited` service message.
    TopicEdited {
        thread_id: Option<i64>,
        message_id: i64,
    },
    /// A message from an allowlisted user that is not a command.
    Message(Inbound),
}

#[derive(Debug)]
enum Done {
    Topic {
        job: TopicJob,
        /// `None`: the scheduler stopped.
        delivery: Option<Delivery>,
    },
    Delete(Option<Delivery>),
    /// A reply chunk or a notice.
    Message(Option<Delivery>),
    Title {
        session: String,
        path: String,
        title: Option<String>,
        /// Bytes of `path` scanned so far.
        scanned: u64,
    },
}

/// A job for the dispatch task.
#[derive(Debug)]
enum Work {
    Topic(TopicJob),
    Delete,
    Message,
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

/// First ai-title of the transcript after byte `from`, and the offset up to
/// which complete lines have been scanned (the next call starts there). The
/// title is `None` when there is none yet or the file is not on this machine.
pub fn read_title(path: &str, from: u64) -> (Option<String>, u64) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return (None, from);
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return (None, from);
    }
    let (title, scanned) = first_ai_title(file, TITLE_SCAN_BYTES.saturating_sub(from));
    (title, from + scanned)
}

/// Streams `jsonl` line by line, at most `limit` bytes, until an ai-title.
/// Also returns the bytes of complete lines read: a last line without its
/// newline may still be being written and is scanned again next time.
fn first_ai_title(jsonl: impl Read, limit: u64) -> (Option<String>, u64) {
    let mut reader = BufReader::new(jsonl.take(limit));
    let mut line = Vec::new();
    let mut scanned = 0;
    loop {
        line.clear();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return (None, scanned),
            Ok(read) => read as u64,
        };
        if line.ends_with(b"\n") {
            scanned += read;
        }
        if let Some(title) = transcript::ai_title(&String::from_utf8_lossy(&line)) {
            return (Some(title), scanned);
        }
    }
}

/// One registered agent connection.
struct Conn {
    /// The session it is bound to or waits for.
    session: String,
    host: String,
    claude_pid: Option<u32>,
    to_agent: mpsc::Sender<HubMsg>,
}

pub struct Slots {
    registry: Registry,
    dispatch: mpsc::UnboundedSender<(Work, Op)>,
    options: Options,
    saver: watch::Sender<Option<Arc<Vec<u8>>>>,
    view: watch::Sender<Arc<TopicView>>,
    conns: HashMap<u64, Conn>,
    /// Agents of sessions no SessionStart announced yet: session -> conn.
    pending: HashMap<String, u64>,
    reading: HashSet<String>,
    /// Transcript bytes already scanned for an ai-title: session -> (path, offset).
    scanned: HashMap<String, (String, u64)>,
    delete_warned: bool,
    /// Messages handed to the dispatch task and not answered yet.
    queued_messages: usize,
    overflow_warned: bool,
    grace_until: Instant,
    next_retry: Instant,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: Option<mpsc::UnboundedReceiver<Done>>,
}

impl Slots {
    /// Starts the save and dispatch tasks. Returns the actor and the view
    /// for `/brief`.
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
        let (dispatch, work) = mpsc::unbounded_channel();
        tokio::spawn(dispatch_loop(outbox, work, done_tx.clone()));
        let now = Instant::now();
        let slots = Self {
            registry,
            dispatch,
            saver,
            view,
            conns: HashMap::new(),
            pending: HashMap::new(),
            reading: HashSet::new(),
            scanned: HashMap::new(),
            delete_warned: false,
            queued_messages: 0,
            overflow_warned: false,
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
    }

    fn on_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Registered {
                conn,
                register,
                to_agent,
            } => {
                let session = self.agent_session(&register);
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
                        "agent of an unknown session waits for its SessionStart"
                    );
                    self.pending.insert(session.clone(), conn);
                }
                self.conns.insert(
                    conn,
                    Conn {
                        session,
                        host: register.host,
                        claude_pid: register.claude_pid,
                        to_agent,
                    },
                );
            }
            AgentEvent::Message { conn, msg } => {
                let Some(bound) = self.conns.get(&conn) else {
                    return;
                };
                match msg {
                    AgentMsg::PermissionRequest(_) => {
                        self.registry.set_waiting(&bound.session, true);
                    }
                    AgentMsg::Reply { text } => self.on_reply(conn, &text),
                    _ => debug!(conn, "agent message not routed yet"),
                }
            }
            AgentEvent::Disconnected { conn } => {
                if let Some(gone) = self.conns.remove(&conn) {
                    self.registry.agent_disconnected(&gone.session, conn);
                    if self.pending.get(&gone.session) == Some(&conn) {
                        self.pending.remove(&gone.session);
                    }
                }
            }
        }
    }

    /// The session a registering agent belongs to. Its env session id, unless
    /// that session is over and the registry knows a newer session of the
    /// same claude process: after `/clear` Claude Code keeps the channel
    /// server running with the old id (TASK-013).
    fn agent_session(&self, register: &crate::wire::Register) -> String {
        if !self.registry.is_live_top_level(&register.session_id)
            && let Some(current) = register
                .claude_pid
                .and_then(|pid| self.registry.live_session_of_pid(&register.host, pid))
        {
            return current.to_owned();
        }
        register.session_id.clone()
    }

    /// Moves the agents of a claude process to the session that process now
    /// runs. Nothing moves when the pid names no running top-level session.
    fn follow_pid(&mut self, host: &str, pid: u32) {
        let Some(session) = self
            .registry
            .live_session_of_pid(host, pid)
            .map(str::to_owned)
        else {
            return;
        };
        let movers: Vec<u64> = self
            .conns
            .iter()
            .filter(|(_, bound)| {
                bound.host == host && bound.claude_pid == Some(pid) && bound.session != session
            })
            .map(|(conn, _)| *conn)
            .collect();
        for conn in movers {
            let Some(bound) = self.conns.get_mut(&conn) else {
                continue;
            };
            let old = std::mem::replace(&mut bound.session, session.clone());
            self.registry.agent_disconnected(&old, conn);
            if self.pending.get(&old) == Some(&conn) {
                self.pending.remove(&old);
            }
            if self.registry.agent_connected(&session, conn) {
                info!(
                    conn,
                    from = short(&old),
                    session = short(&session),
                    "agent follows its claude process to a new session"
                );
            }
        }
    }

    fn on_hook(&mut self, post: &HookPost) {
        let followup = self.registry.apply_hook(post);
        let session = post.session_id.as_str();
        if matches!(post.event, HookEvent::SessionEnd { .. }) {
            self.scanned.remove(session);
        }
        if self.registry.sessions.contains_key(session)
            && let Some(conn) = self.pending.remove(session)
        {
            self.registry.agent_connected(session, conn);
        }
        if let HookEvent::SessionStart {
            claude_pid: Some(pid),
            ..
        } = post.event
        {
            self.follow_pid(&post.host, pid);
        }
        if let Some((session, path)) = followup.read_title {
            self.read_title(session, path);
        }
    }

    fn read_title(&mut self, session: String, path: String) {
        if !self.reading.insert(session.clone()) {
            return;
        }
        // Only the tail written since the last scan of the same file.
        let from = match self.scanned.get(&session) {
            Some((scanned_path, offset)) if *scanned_path == path => *offset,
            _ => 0,
        };
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let read_path = path.clone();
            let (title, scanned) =
                tokio::task::spawn_blocking(move || read_title(&read_path, from))
                    .await
                    .unwrap_or((None, from));
            let _ = done.send(Done::Title {
                session,
                path,
                title,
                scanned,
            });
        });
    }

    fn on_control(&mut self, control: Control) {
        let (thread_id, message_id) = match control {
            Control::TopicEdited {
                thread_id,
                message_id,
            } => (thread_id, message_id),
            Control::Message(input) => return self.on_topic_message(input),
        };
        if !self.options.can_delete
            || thread_id
                .and_then(|t| self.registry.slot_by_topic(t))
                .is_none()
        {
            return;
        }
        self.hand_off(Work::Delete, Op::Delete { message_id });
    }

    /// Forwards a topic message to the agent of the slot's current session,
    /// or tells the user it did not get there. General and topics that are
    /// not slots reach no agent and get no answer.
    fn on_topic_message(&mut self, input: Inbound) {
        let Some(thread_id) = input.thread_id else {
            debug!("message outside a topic; not forwarded");
            return;
        };
        let Some(slot) = self.registry.slot_by_topic(thread_id) else {
            debug!("message in a topic without a slot; not forwarded");
            return;
        };
        let ordinal = self.ordinal(slot);
        let Some(content) = input.text else {
            self.notify(thread_id, TEXT_ONLY_NOTICE);
            return;
        };
        let Some((session, conn)) = self.live_agent(slot) else {
            info!(
                ordinal,
                "message for a session that is not on line; user notified"
            );
            self.notify(thread_id, OFFLINE_NOTICE);
            return;
        };
        let mut meta = BTreeMap::from([
            ("chat_id".to_owned(), self.options.chat_id.to_string()),
            ("message_id".to_owned(), input.message_id.to_string()),
            ("thread_id".to_owned(), thread_id.to_string()),
        ]);
        if let Some(reply_to) = input.reply_to {
            meta.insert("reply_to_message_id".to_owned(), reply_to.to_string());
        }
        let inbound = HubMsg::Inbound { content, meta };
        let sent = self
            .conns
            .get(&conn)
            .is_some_and(|bound| bound.to_agent.try_send(inbound).is_ok());
        if sent {
            info!(
                ordinal,
                session = short(&session),
                "message forwarded to the session agent"
            );
        } else {
            warn!(
                ordinal,
                session = short(&session),
                "agent queue full or closed; user notified"
            );
            self.notify(thread_id, OFFLINE_NOTICE);
        }
    }

    /// The running top-level session of `slot` and its agent connection.
    fn live_agent(&self, slot: SlotId) -> Option<(String, u64)> {
        let session = self.registry.slot(slot)?.current_session.as_deref()?;
        if !self.registry.is_live_top_level(session) {
            return None;
        }
        let conn = self.registry.sessions.get(session)?.agent?;
        self.conns
            .contains_key(&conn)
            .then(|| (session.to_owned(), conn))
    }

    /// Sends an agent's reply to the topic of its session: the chunks of
    /// `split_for_telegram` in order, or one document when it prefers a file.
    fn on_reply(&mut self, conn: u64, text: &str) {
        let Some(bound) = self.conns.get(&conn) else {
            return;
        };
        let session = bound.session.clone();
        let slot = self
            .registry
            .sessions
            .get(&session)
            .filter(|entry| entry.agent == Some(conn))
            .and_then(|entry| entry.slot);
        let Some(slot) = slot else {
            debug!(
                conn,
                "reply from an agent that is not bound to a slot; dropped"
            );
            return;
        };
        let ordinal = self.ordinal(slot);
        let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) else {
            warn!(ordinal, "reply for a slot without a topic yet; dropped");
            return;
        };
        let split = split_for_telegram(text, SplitOptions::default());
        let ops: Vec<Op> = if split.prefer_file {
            vec![Op::SendDocument {
                thread_id: Some(thread_id),
                document: Document {
                    file_name: format!("reply-{}.txt", short(&session)),
                    bytes: text.as_bytes().to_vec(),
                    caption: None,
                },
            }]
        } else {
            split
                .chunks
                .into_iter()
                .map(|chunk| message_op(thread_id, chunk))
                .collect()
        };
        let parts = ops.len();
        if self.send_messages(ops) {
            info!(
                ordinal,
                session = short(&session),
                parts,
                "agent reply queued"
            );
        }
    }

    fn notify(&mut self, thread_id: i64, text: &str) {
        self.send_messages(vec![message_op(thread_id, text.to_owned())]);
    }

    /// Hands all `ops` to the dispatch task in order, or none of them when
    /// that would pass [`MAX_QUEUED_MESSAGES`].
    fn send_messages(&mut self, ops: Vec<Op>) -> bool {
        if self.queued_messages + ops.len() > MAX_QUEUED_MESSAGES {
            if !self.overflow_warned {
                self.overflow_warned = true;
                warn!("too many messages wait for Telegram; new replies and notices are dropped");
            }
            return false;
        }
        self.queued_messages += ops.len();
        for op in ops {
            self.hand_off(Work::Message, op);
        }
        true
    }

    /// Never waits: the dispatch task does.
    fn hand_off(&self, work: Work, op: Op) {
        if self.dispatch.send((work, op)).is_err() {
            debug!("dispatch task stopped; job dropped");
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
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
            Done::Message(delivery) => {
                self.queued_messages = self.queued_messages.saturating_sub(1);
                if self.queued_messages == 0 {
                    self.overflow_warned = false;
                }
                match delivery {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => warn!(%error, "message to a topic not delivered"),
                    None => warn!("message to a topic got no answer"),
                }
            }
            Done::Title {
                session,
                path,
                title,
                scanned,
            } => {
                self.reading.remove(&session);
                match title {
                    Some(title) => {
                        self.scanned.remove(&session);
                        self.registry.set_title(&session, &title);
                    }
                    // A session that ended during the scan is not scanned again.
                    None if self
                        .registry
                        .sessions
                        .get(&session)
                        .is_some_and(|entry| !entry.ended) =>
                    {
                        self.scanned.insert(session, (path, scanned));
                    }
                    None => {}
                }
            }
        }
    }

    fn on_topic_done(&mut self, job: TopicJob, delivery: Option<Delivery>) {
        let icons = &self.options.icons;
        let Some(delivery) = delivery else {
            // The scheduler stopped: handing the job out again would come
            // back at once, so it waits for the retry tick like a failure.
            let (TopicJob::Create { slot, .. }
            | TopicJob::Edit { slot, .. }
            | TopicJob::Separator { slot, .. }) = job;
            warn!(
                ordinal = self.ordinal(slot),
                "topic call got no answer; retrying later"
            );
            self.registry.topic_failed(slot, icons);
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
                    // Known limitation (TASK-011 review I3): createForumTopic
                    // has no idempotency key. If Telegram made the topic but
                    // the answer was lost (or the hub died before saving it),
                    // the retry makes a second one and the first is orphaned.
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
                    // Known limitation (TASK-011 QA O1): a separator that
                    // always fails is retried every tick and holds back the
                    // slot's name and icon edits, which come after it.
                    if let Err(error) = &delivery {
                        warn!(%error, ordinal = self.ordinal(slot), "session separator not delivered; retrying later");
                    }
                    self.registry.topic_failed(slot, icons);
                }
            }
        }
    }

    /// Hands pending topic work to the dispatch task, publishes the view and
    /// the snapshot to save.
    fn pump(&mut self) {
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
            self.hand_off(Work::Topic(job), op);
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

fn message_op(thread_id: i64, text: String) -> Op {
    Op::Send {
        thread_id: Some(thread_id),
        text,
        reply_markup: None,
        permission: false,
    }
}

/// Enqueues jobs in order, waiting for room in the scheduler queue so the
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
                Work::Message => Done::Message(delivery),
            });
        });
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
    // Upper bound for events a test expects; a stall shows up as a hang, so a
    // generous bound only protects against a loaded host, it hides nothing.
    const WAIT: Duration = Duration::from_secs(60);

    /// Records every op. Topics are numbered from 100. `edit_errors` and
    /// `send_errors` answer the next edits or sends with these Telegram
    /// errors (last first). `stall`: no call ever returns.
    #[derive(Default)]
    struct Fake {
        ops: Mutex<Vec<Op>>,
        next_topic: Mutex<i64>,
        edit_errors: Mutex<Vec<&'static str>>,
        send_errors: Mutex<Vec<&'static str>>,
        delete_error: Option<&'static str>,
        stall: bool,
        /// Only sends never return; topic calls still answer.
        stall_sends: bool,
    }

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            self.ops.lock().unwrap().push(op.clone());
            let send = matches!(op, Op::Send { .. } | Op::SendDocument { .. });
            if self.stall || (self.stall_sends && send) {
                return std::future::pending().await;
            }
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
                Op::Send { .. } => match self.send_errors.lock().unwrap().pop() {
                    Some(description) => error(description),
                    None => Ok(Outcome::Sent(Message::default())),
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
            self.agent_of(conn, session, None).await;
        }

        /// An agent that reports the pid of its claude process.
        async fn agent_of(&mut self, conn: u64, session: &str, claude_pid: Option<u32>) {
            let (to_agent, rx) = mpsc::channel(4);
            self._to_agent.push(rx);
            let register = Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
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

    const CHAT: i64 = -1000000000001;

    fn say(thread_id: Option<i64>, message_id: i64, text: Option<&str>) -> Control {
        Control::Message(Inbound {
            message_id,
            thread_id,
            text: text.map(str::to_owned),
            reply_to: None,
        })
    }

    fn message_options() -> Options {
        Options {
            chat_id: CHAT,
            ..options()
        }
    }

    /// What agent `index` (in registration order) received, after a pause.
    async fn received(rig: &mut Rig, index: usize) -> Vec<HubMsg> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut got = Vec::new();
        while let Ok(msg) = rig._to_agent[index].try_recv() {
            got.push(msg);
        }
        got
    }

    fn sent_to(ops: &[Op], thread: i64) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                } if *t == thread => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Topic 100 for A (agent 0, conn 1) and, with `second`, topic 101 for B
    /// (agent 1, conn 2); waits until both are alive.
    async fn two_live_slots(rig: &mut Rig, second: bool) {
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        if second {
            rig.hook(start(B, 11)).await;
            settled(rig, |ops| count(ops, is_create) == 2).await;
            rig.agent_of(2, B, Some(11)).await;
            settled(rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        }
        settled(rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn a_topic_message_reaches_only_the_agent_of_its_slot() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let before = rig.fake.ops().len();
        rig.control.send(say(Some(101), 42, Some("hi B"))).unwrap();
        rig.control
            .send(Control::Message(Inbound {
                message_id: 43,
                thread_id: Some(101),
                text: Some("again".into()),
                reply_to: Some(40),
            }))
            .unwrap();
        // General and a topic that is no slot reach nobody and say nothing.
        rig.control.send(say(None, 44, Some("general"))).unwrap();
        rig.control
            .send(say(Some(999), 45, Some("foreign")))
            .unwrap();

        let got = received(&mut rig, 1).await;
        let meta = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        assert_eq!(
            got,
            [
                HubMsg::Inbound {
                    content: "hi B".into(),
                    meta: meta(&[
                        ("chat_id", "-1000000000001"),
                        ("message_id", "42"),
                        ("thread_id", "101"),
                    ]),
                },
                HubMsg::Inbound {
                    content: "again".into(),
                    meta: meta(&[
                        ("chat_id", "-1000000000001"),
                        ("message_id", "43"),
                        ("reply_to_message_id", "40"),
                        ("thread_id", "101"),
                    ]),
                },
            ]
        );
        for msg in &got {
            if let HubMsg::Inbound { meta, .. } = msg {
                assert!(meta.keys().all(|key| crate::channel::is_meta_key(key)));
            }
        }
        assert!(received(&mut rig, 0).await.is_empty(), "A got nothing");
        assert_eq!(
            rig.fake.ops().len(),
            before,
            "no notice: {:?}",
            rig.fake.ops()
        );
    }

    #[tokio::test]
    async fn a_message_nobody_can_take_gets_one_notice_each() {
        let mut rig = rig(Fake::default(), message_options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // A session without an agent.
        rig.control.send(say(Some(100), 1, Some("one"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 1).await;
        // An agent whose link queue is gone.
        rig.agent_of(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig._to_agent.clear();
        rig.control.send(say(Some(100), 2, Some("two"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 2).await;
        // A photo: only text is forwarded.
        rig.control.send(say(Some(100), 3, None)).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            sent_to(&rig.fake.ops(), 100),
            [OFFLINE_NOTICE, OFFLINE_NOTICE, TEXT_ONLY_NOTICE]
        );
    }

    #[tokio::test]
    async fn an_ended_session_gets_no_inbound_even_with_its_agent_still_linked() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100) == [OFFLINE_NOTICE]).await;
        assert!(received(&mut rig, 0).await.is_empty());
    }

    #[tokio::test]
    async fn after_clear_the_new_session_of_the_slot_gets_the_message() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        settled(&rig, |ops| {
            sent_to(ops, 100).contains(&"── session bbbbbbbb · new ──")
        })
        .await;
        rig.control
            .send(say(Some(100), 7, Some("after clear")))
            .unwrap();
        let got = received(&mut rig, 0).await;
        assert!(
            matches!(got.as_slice(), [HubMsg::Inbound { content, .. }] if content == "after clear"),
            "{got:?}"
        );
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
    }

    fn reply(conn: u64, text: &str) -> AgentEvent {
        AgentEvent::Message {
            conn,
            msg: AgentMsg::Reply { text: text.into() },
        }
    }

    #[tokio::test]
    async fn a_reply_goes_to_its_session_topic_in_split_order() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let paragraph = |c: char| format!("{}\n\n", c.to_string().repeat(3000));
        let long: String = ['a', 'b', 'c'].into_iter().map(paragraph).collect();
        let want = split_for_telegram(&long, SplitOptions::default());
        assert!(want.chunks.len() > 1 && !want.prefer_file);
        rig.agents.send(reply(2, &long)).await.unwrap();
        rig.agents.send(reply(2, "short")).await.unwrap();
        let expected: Vec<&str> = want
            .chunks
            .iter()
            .map(String::as_str)
            .chain(["short"])
            .collect();
        let ops = settled(&rig, |ops| sent_to(ops, 101).len() == expected.len()).await;
        assert_eq!(sent_to(&ops, 101), expected);
        assert!(sent_to(&ops, 100).is_empty());

        // More chunks than `max_chunks`: one document with the whole text.
        let huge = "x".repeat(5 * 4096);
        rig.agents.send(reply(1, &huge)).await.unwrap();
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::SendDocument { .. }))
        })
        .await;
        assert!(ops.iter().any(|op| matches!(op,
            Op::SendDocument { thread_id: Some(100), document } if document.bytes == huge.as_bytes())));
    }

    #[tokio::test]
    async fn a_reply_from_an_agent_without_a_slot_is_dropped() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agent(5, B).await; // no SessionStart for B: waits, unbound
        let before = rig.fake.ops().len();
        rig.agents.send(reply(5, "lost")).await.unwrap();
        rig.agents.send(reply(99, "unknown conn")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(rig.fake.ops().len(), before, "{:?}", rig.fake.ops());
    }

    #[tokio::test]
    async fn a_stalled_telegram_never_stalls_inbound() {
        let fake = Fake {
            stall_sends: true,
            ..Fake::default()
        };
        let mut rig = rig(fake, message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(start(B, 11)).await; // slot 101, no agent: notices
        settled(&rig, |ops| count(ops, is_create) == 2).await;
        // Far more replies and notices than the scheduler queue (1024) and
        // the cap: every one of them is stuck behind a send that never ends.
        let flood = async {
            for i in 0..1500 {
                rig.agents.send(reply(1, "stuck")).await.unwrap();
                rig.control.send(say(Some(101), i, Some("x"))).unwrap();
            }
        };
        tokio::time::timeout(WAIT, flood)
            .await
            .expect("agent events kept draining");
        rig.control
            .send(say(Some(100), 5000, Some("still here")))
            .unwrap();
        let arrived = async {
            loop {
                if let Some(HubMsg::Inbound { content, .. }) = rig._to_agent[0].recv().await
                    && content == "still here"
                {
                    return;
                }
            }
        };
        tokio::time::timeout(WAIT, arrived)
            .await
            .expect("inbound reached the agent while Telegram stalled");
    }

    #[tokio::test]
    async fn the_backlog_of_messages_for_telegram_is_capped() {
        let dir = TempDir::new("slots-cap");
        let store = RegistryStore::open(dir.path()).unwrap();
        let stalled = Fake {
            stall: true,
            ..Fake::default()
        };
        let (scheduler, outbox) = Scheduler::new(Arc::new(stalled), BucketConfig::default());
        tokio::spawn(scheduler.run());
        let (mut slots, _view) = Slots::new(Registry::default(), store, outbox, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "t", None);
        for i in 0..MAX_QUEUED_MESSAGES as i64 + 50 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
        assert!(slots.overflow_warned);
        // An answer frees one place and the next notice takes it.
        slots.on_done(Done::Message(None));
        slots.on_control(say(Some(100), 1000, Some("x")));
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
    }

    fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {
        ops.iter().filter(|op| pred(op)).count()
    }

    fn is_create(op: &Op) -> bool {
        matches!(op, Op::CreateTopic { .. })
    }

    #[tokio::test]
    async fn the_agent_follows_its_claude_process_through_clear() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(icon_edit(&ops[1]), Some(ICON_ALIVE), "{ops:?}");

        // `/clear`: the channel server keeps running with the old id.
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        // The end may flash "dead" before the start arrives; what matters is
        // that the new session ends up alive, never "no channel".
        let ops = rig.ops_after(5).await;
        assert_eq!(count(&ops, is_create), 1, "same slot: {ops:?}");
        assert!(
            ops[1..]
                .iter()
                .all(|op| icon_edit(op) != Some(ICON_NO_CHANNEL)),
            "{ops:?}"
        );
        let last_icon = ops.iter().rev().find_map(icon_edit);
        assert_eq!(last_icon, Some(ICON_ALIVE), "{ops:?}");

        // The link drops and comes back with the stale env id: still B.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        let before = rig.ops_after(ops.len() + 1).await.len();
        rig.agent_of(2, A, Some(10)).await;
        let ops = rig.ops_after(before + 1).await;
        assert_eq!(icon_edit(ops.last().unwrap()), Some(ICON_ALIVE), "{ops:?}");
    }

    /// The recorded ops, once they satisfy `ready`.
    async fn settled(rig: &Rig, ready: impl Fn(&[Op]) -> bool) -> Vec<Op> {
        let reached = async {
            loop {
                let ops = rig.fake.ops();
                if ready(&ops) {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        match tokio::time::timeout(WAIT, reached).await {
            Ok(ops) => ops,
            Err(_) => panic!("ops never settled: {:?}", rig.fake.ops()),
        }
    }

    fn last_icon(ops: &[Op], thread: i64) -> Option<&str> {
        ops.iter().rev().find_map(|op| match op {
            Op::EditTopic {
                thread_id,
                icon_custom_emoji_id: Some(icon),
                ..
            } if *thread_id == thread => Some(icon.as_str()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn the_agent_follows_its_claude_process_when_the_new_start_comes_first() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.ops_after(2).await;

        // `/clear` hooks run as separate processes: the new start can win.
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        // B takes A's slot (same claude pid) and the agent moves with it:
        // the topic is renamed and never shows "no channel".
        let named_b = |ops: &[Op]| {
            ops.iter().any(|op| {
                matches!(op, Op::EditTopic { thread_id: 100, name: Some(name), .. }
                    if name.ends_with("bbbbbbbb"))
            })
        };
        settled(&rig, named_b).await;
        // Room for a wrong "no channel" edit to show up.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = rig.fake.ops();
        assert_eq!(count(&ops, is_create), 1, "{ops:?}");
        assert!(
            ops.iter().all(|op| icon_edit(op) != Some(ICON_NO_CHANNEL)),
            "{ops:?}"
        );
        assert_eq!(last_icon(&ops, 100), Some(ICON_ALIVE), "{ops:?}");

        // The late end of A must not take the pid away from B: the link
        // drops (B loses its agent) and a reconnect with the stale env id
        // lands on B again.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_NO_CHANNEL)).await;
        rig.agent_of(2, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
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

        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
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

        // No hook yet: the agent waits, however long, and makes no topic.
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

    #[test]
    fn the_ai_title_is_found_past_the_head_of_a_long_transcript() {
        let filler = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n";
        let mut jsonl = filler.repeat(5 * 1024 * 1024 / filler.len() + 1);
        assert!(jsonl.len() > 5 * 1024 * 1024);
        jsonl.push_str("{\"type\":\"ai-title\",\"aiTitle\":\"Late title\"}\n");
        let dir = TempDir::new("slots-title");
        let path = dir.path().join("long.jsonl");
        std::fs::write(&path, &jsonl).unwrap();
        assert_eq!(
            read_title(&path.display().to_string(), 0),
            (Some("Late title".to_owned()), jsonl.len() as u64)
        );
        // Past the cap: not read.
        assert_eq!(
            first_ai_title(jsonl.as_bytes(), (jsonl.len() - 10) as u64).0,
            None
        );
        assert_eq!(
            read_title(&dir.path().join("none.jsonl").display().to_string(), 7),
            (None, 7)
        );
    }

    #[test]
    fn a_title_scan_goes_on_from_where_the_last_one_stopped() {
        let dir = TempDir::new("slots-title-tail");
        let path = dir.path().join("t.jsonl");
        let path_text = path.display().to_string();
        let head = "{\"type\":\"ai-title\",\"aiTitle\":\"Head\"}\n{\"type\":\"user\"}\n";
        std::fs::write(&path, head).unwrap();
        // Scanning from the end of what was read skips the head entirely.
        let end = head.len() as u64;
        assert_eq!(read_title(&path_text, end), (None, end));

        // A line still being written is not counted as scanned.
        let partial = "{\"type\":\"ai-ti";
        std::fs::write(&path, format!("{head}{partial}")).unwrap();
        assert_eq!(read_title(&path_text, end), (None, end));

        let tail = format!("{partial}tle\",\"aiTitle\":\"Tail\"}}\n");
        std::fs::write(&path, format!("{head}{tail}")).unwrap();
        assert_eq!(
            read_title(&path_text, end),
            (Some("Tail".to_owned()), end + tail.len() as u64)
        );
    }

    #[tokio::test]
    async fn a_stopped_scheduler_is_retried_on_the_tick_not_in_a_loop() {
        let dir = TempDir::new("slots-stopped");
        let store = RegistryStore::open(dir.path()).unwrap();
        let (scheduler, outbox) =
            Scheduler::new(Arc::new(Fake::default()), BucketConfig::default());
        drop(scheduler); // every submit now comes back without an answer
        let (mut slots, _view) = Slots::new(Registry::default(), store, outbox, options());
        let mut done = slots.done_rx.take().unwrap();
        slots.on_hook(&start(A, 10));
        slots.pump();
        let answer = tokio::time::timeout(WAIT, done.recv()).await.unwrap();
        assert!(
            matches!(answer, Some(Done::Topic { delivery: None, .. })),
            "{answer:?}"
        );
        slots.on_done(answer.unwrap());
        slots.pump();
        let again = tokio::time::timeout(Duration::from_millis(300), done.recv()).await;
        assert!(again.is_err(), "handed out again at once: {again:?}");
        // The retry tick hands it out once more.
        slots.next_retry = Instant::now();
        slots.on_tick();
        slots.pump();
        let retried = tokio::time::timeout(WAIT, done.recv()).await.unwrap();
        assert!(matches!(retried, Some(Done::Topic { delivery: None, .. })));
    }

    #[tokio::test]
    async fn a_title_scan_that_ends_after_session_end_keeps_nothing() {
        let dir = TempDir::new("slots-scan-end");
        let store = RegistryStore::open(dir.path()).unwrap();
        let (_scheduler, outbox) =
            Scheduler::new(Arc::new(Fake::default()), BucketConfig::default());
        let (mut slots, _view) = Slots::new(Registry::default(), store, outbox, options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        slots.on_done(Done::Title {
            session: A.into(),
            path: "t.jsonl".into(),
            title: None,
            scanned: 10,
        });
        assert!(slots.scanned.is_empty());
    }

    #[tokio::test]
    async fn a_title_less_transcript_is_scanned_only_past_the_last_scan() {
        let rig = rig(Fake::default(), options());
        let transcript = rig.dir.path().join("t.jsonl");
        let head = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        std::fs::write(&transcript, head).unwrap();
        let mut first = start(A, 10);
        first.transcript_path = transcript.display().to_string();
        rig.hook(first).await;
        rig.ops_after(1).await;
        let stop = || {
            hook(
                A,
                HookEvent::Stop {
                    prompt_id: None,
                    last_assistant_message: None,
                },
            )
        };
        rig.hook(stop()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // A title placed inside the part already scanned is never seen again:
        // proof that the next scan starts past it. One appended later is.
        let mut hidden = String::from("{\"type\":\"ai-title\",\"aiTitle\":\"Old\"}");
        hidden.push_str(&" ".repeat(head.len() - hidden.len() - 1));
        hidden.push('\n');
        assert_eq!(hidden.len(), head.len());
        std::fs::write(
            &transcript,
            format!("{hidden}{{\"type\":\"ai-title\",\"aiTitle\":\"New\"}}\n"),
        )
        .unwrap();
        rig.hook(stop()).await;
        let ops = rig.ops_after(2).await;
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), .. }
            if name == "[box] Project · New"),
            "{ops:?}"
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

    fn sends(ops: &[Op]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn unknown_sessions_make_no_topics_until_their_session_start() {
        let mut rig = rig(Fake::default(), options());
        // Prompt hooks and an agent of a session no SessionStart announced.
        for event in [
            HookEvent::UserPromptSubmit { prompt_id: None },
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ] {
            rig.hook(hook(A, event)).await;
        }
        rig.agent(1, A).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(rig.fake.ops().is_empty(), "{:?}", rig.fake.ops());
        // Its SessionStart proves top-level: one topic, the agent bound.
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(
            matches!(&ops[0], Op::CreateTopic { icon_custom_emoji_id, .. }
            if icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
    }

    #[tokio::test]
    async fn a_stalled_telegram_never_stalls_ingress() {
        let fake = Fake {
            stall: true,
            ..Fake::default()
        };
        let rig = rig(fake, options());
        // More topics than the scheduler queue holds (1024) plus the hook
        // channel (16): an actor that waits for the queue stops reading.
        const SESSIONS: u32 = 1100;
        let sent = async {
            for i in 0..SESSIONS {
                let mut post = start(&format!("s{i:05}"), 1000 + i);
                post.cwd = format!(r"C:\Work\P{i}");
                rig.hook(post).await;
            }
        };
        tokio::time::timeout(WAIT, sent)
            .await
            .expect("ingress kept draining while Telegram stalled");
        let saved = async {
            loop {
                let store = RegistryStore::open(rig.dir.path()).unwrap();
                if store
                    .load()
                    .is_ok_and(|saved| saved.slots.len() == SESSIONS as usize)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(WAIT, saved)
            .await
            .expect("every session registered");
    }

    #[tokio::test]
    async fn a_failed_separator_is_sent_again() {
        let fake = Fake {
            send_errors: Mutex::new(vec!["Bad Request: something else"]),
            ..Fake::default()
        };
        let rig = rig(
            fake,
            Options {
                retry_every: Duration::from_millis(100),
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        rig.hook(start(B, 11)).await;
        let separator = "── session bbbbbbbb · new ──";
        let delivered = async {
            while sends(&rig.fake.ops()).len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, delivered)
            .await
            .expect("the separator is tried again");
        tokio::time::sleep(Duration::from_millis(400)).await;
        let ops = rig.fake.ops();
        assert_eq!(sends(&ops), [separator, separator], "{ops:?}");
    }

    #[tokio::test]
    async fn a_gone_topic_during_a_session_change_is_replaced_once() {
        let gone = "Bad Request: TOPIC_ID_INVALID";
        let fake = Fake {
            edit_errors: Mutex::new(vec![gone]),
            send_errors: Mutex::new(vec![gone]),
            ..Fake::default()
        };
        let rig = rig(fake, options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // `/clear` in A's process: one event changes the session (separator)
        // and the title (edit) of the slot whose topic was deleted.
        let mut clear = start(B, 10);
        clear.event = HookEvent::SessionStart {
            source: Some("clear".into()),
            claude_pid: Some(10),
            parent_claude_pid: None,
        };
        rig.hook(clear).await;
        let ops = rig.ops_after(3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = if ops.len() < rig.fake.ops().len() {
            rig.fake.ops()
        } else {
            ops
        };
        assert_eq!(count(&ops, is_create), 2, "one replacement: {ops:?}");
        let replacement = ops.iter().rposition(is_create).unwrap();
        assert!(
            !ops[replacement..].iter().any(|op| matches!(
                op,
                Op::EditTopic { thread_id: 100, .. }
                    | Op::Send {
                        thread_id: Some(100),
                        ..
                    }
            )),
            "{ops:?}"
        );
    }
}

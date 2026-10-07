//! Status message end to end (TASK-029): hook events over real HTTP to the
//! real `serve_hooks`, an agent over a real TCP link to the real
//! `serve_agents` (a hand-written client that announces `console_keys` and
//! answers `console_key`), the real `Slots` actor and `Scheduler`, and a fake
//! Telegram transport. Button presses come in as the update poll hands them
//! over (`Control::Callback`); which updates get that far (allowlist, the
//! bot's own pin notices) is tested in `hub::updates` and `hub::mod`.
//! The compaction test (TASK-053) runs the real `cctg hook PreCompact`.

use std::collections::VecDeque;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hook;
use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat};
use cctg::hub::ingress::{bind, serve_agents, serve_hooks};
use cctg::hub::registry::{ICON_ALIVE, RegistryStore};
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::status;
use cctg::hub::updates::{CallbackInput, Inbound};
use cctg::hub::{console, stream};
use cctg::wire::{
    self, AgentMsg, CommandOutcome, ConsoleKey, HookEvent, HookPost, HubMsg, PermissionRequest,
    Register, Secret,
};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The default group of this test (TASK-069): the chat its Bot API fake
/// and its registry name.
const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));

mod common;

const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const CWD: &str = "C:/qa/status";
const A: &str = "0a16e2e0-0000-4000-8000-000000000291";
const B: &str = "0a16e2e0-0000-4000-8000-000000000292";
const WAIT: Duration = Duration::from_secs(30);
const NUMBERS: &str = "Opus 5.5 · high · ctx 50% · 5h 3% · 7d 92%";

// ---------------------------------------------------------------- fake Telegram

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Op>>,
    next_topic: AtomicI64,
    next_message: AtomicI64,
    /// The next message edits fail with these descriptions (front first).
    edit_errors: Mutex<VecDeque<&'static str>>,
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100 + self.next_topic.fetch_add(1, Ordering::SeqCst),
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } => Ok(Outcome::Sent(Message {
                message_id: 1000 + self.next_message.fetch_add(1, Ordering::SeqCst),
                ..Message::default()
            })),
            Op::Edit { .. } => match self.edit_errors.lock().unwrap().pop_front() {
                Some(description) => Err(ApiError::Telegram {
                    code: 400,
                    description: description.into(),
                }),
                None => Ok(Outcome::Done),
            },
            _ => Ok(Outcome::Done),
        }
    }
}

impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }
}

/// Status message sends: (topic, text). Permission prompts carry buttons
/// too and are not status messages.
fn status_sends(ops: &[Op]) -> Vec<(i64, String)> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Send {
                thread_id: Some(thread),
                text,
                reply_markup: Some(_),
                permission: false,
                ..
            } => Some((*thread, text.clone())),
            _ => None,
        })
        .collect()
}

/// (message id, request id) of every permission prompt sent. Sends are
/// numbered from 1000 in call order.
fn prompts(ops: &[Op]) -> Vec<(i64, String)> {
    ops.iter()
        .filter(|op| matches!(op, Op::Send { .. }))
        .enumerate()
        .filter_map(|(index, op)| match op {
            Op::Send {
                permission: true,
                reply_markup: Some(markup),
                ..
            } => {
                let data = markup["inline_keyboard"][0][0]["callback_data"].as_str()?;
                let id = data.strip_prefix("allow:")?.to_owned();
                Some((1000 + index as i64, id))
            }
            _ => None,
        })
        .collect()
}

/// TASK-062: the messages topic `thread` shows now, oldest first: sends
/// (numbered from 1000 in call order) that were not deleted, with their
/// text and buttons as their last edit left them.
fn topic(ops: &[Op], thread: i64) -> Vec<(i64, String, Vec<String>)> {
    let buttons = |markup: &Option<serde_json::Value>| -> Vec<String> {
        markup
            .as_ref()
            .and_then(|markup| markup["inline_keyboard"].as_array())
            .into_iter()
            .flatten()
            .flat_map(|row| row.as_array().into_iter().flatten())
            .filter_map(|button| button["callback_data"].as_str().map(str::to_owned))
            .collect()
    };
    let mut shown: Vec<(i64, String, Vec<String>)> = Vec::new();
    let mut id = 1000;
    for op in ops {
        match op {
            Op::Send {
                thread_id,
                text,
                reply_markup,
                ..
            } => {
                if *thread_id == Some(thread) {
                    shown.push((id, text.clone(), buttons(reply_markup)));
                }
                id += 1;
            }
            Op::Edit {
                message_id,
                text,
                reply_markup,
                ..
            } => {
                if let Some(message) = shown.iter_mut().find(|(at, ..)| at == message_id) {
                    message.1.clone_from(text);
                    message.2 = buttons(reply_markup);
                }
            }
            Op::Delete { message_id, .. } => shown.retain(|(at, ..)| at != message_id),
            _ => {}
        }
    }
    shown
}

/// The status message of topic `thread` now: the newest status send (the
/// older ones are deleted when it moves), and what it shows.
fn current_status(ops: &[Op], thread: i64) -> Option<(i64, (String, Vec<String>))> {
    let id = ops
        .iter()
        .filter(|op| matches!(op, Op::Send { .. }))
        .zip(1000..)
        .filter_map(|(op, id)| match op {
            Op::Send {
                thread_id: Some(t),
                reply_markup: Some(_),
                permission: false,
                ..
            } if *t == thread => Some(id),
            _ => None,
        })
        .last()?;
    let (_, text, buttons) = topic(ops, thread).into_iter().find(|(at, ..)| *at == id)?;
    Some((id, (text, buttons)))
}

/// TASK-062: the status message is the last message of its topic, and
/// nothing is pinned.
fn assert_status_last(ops: &[Op], thread: i64) {
    let (status, _) = current_status(ops, thread).expect("a status message");
    assert_eq!(
        topic(ops, thread).last().map(|(id, ..)| *id),
        Some(status),
        "{:#?}",
        topic(ops, thread)
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::Unpin { .. })),
        "{ops:#?}"
    );
}

/// The icon topic `thread` shows: set at creation (topics are numbered from
/// 100 in creation order), then by its last icon edit.
fn topic_icon(ops: &[Op], thread: i64) -> Option<&str> {
    let mut created = 100;
    let mut icon = None;
    for op in ops {
        match op {
            Op::CreateTopic {
                icon_custom_emoji_id,
                ..
            } => {
                if created == thread {
                    icon = icon_custom_emoji_id.as_deref();
                }
                created += 1;
            }
            Op::EditTopic {
                thread_id,
                icon_custom_emoji_id: Some(id),
                ..
            } if *thread_id == thread => icon = Some(id),
            _ => {}
        }
    }
    icon
}

/// Edits of `message`: text and the callback data of its buttons.
fn edits(ops: &[Op], message: i64) -> Vec<(String, Vec<String>)> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Edit {
                message_id,
                text,
                reply_markup,
                ..
            } if *message_id == message => {
                let buttons = reply_markup
                    .as_ref()
                    .and_then(|markup| markup["inline_keyboard"].as_array())
                    .into_iter()
                    .flatten()
                    .flat_map(|row| row.as_array().into_iter().flatten())
                    .filter_map(|button| button["callback_data"].as_str().map(str::to_owned))
                    .collect();
                Some((text.clone(), buttons))
            }
            _ => None,
        })
        .collect()
}

fn shown(text: &str, buttons: &[&str]) -> (String, Vec<String>) {
    (
        text.to_owned(),
        buttons.iter().map(|button| (*button).to_owned()).collect(),
    )
}

fn answers(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter_map(|op| match op {
            Op::AnswerCallback { text, .. } => Some(text.clone().unwrap_or_default()),
            _ => None,
        })
        .collect()
}

fn key_failed_notices(ops: &[Op]) -> usize {
    ops.iter()
        .filter(|op| matches!(op, Op::Send { text, .. } if text == status::KEY_FAILED_NOTICE))
        .count()
}

// ---------------------------------------------------------------- hub

struct Hub {
    fake: Arc<Fake>,
    hook_addr: String,
    agent_addr: SocketAddr,
    control: mpsc::UnboundedSender<Control>,
    actor: Option<JoinHandle<()>>,
    tasks: Vec<JoinHandle<()>>,
    state: Option<TempRoot>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in self.tasks.iter().chain(&self.actor) {
            task.abort();
        }
    }
}

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn start_hub(name: &str, every: Duration) -> Hub {
    let state = std::env::temp_dir().join(format!("cctg-status-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    start_hub_in(TempRoot(state), every).await
}

/// A hub on the state (`registry.json`) of `state`.
async fn start_hub_in(state: TempRoot, every: Duration) -> Hub {
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    let store = RegistryStore::open(&state.0).unwrap();
    let registry = store.load(GroupChat::of(GROUP_ID)).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(every),
        ..Options::default()
    };
    let slots = Slots::new(registry, store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let secret = Secret::parse(SECRET).unwrap();
    let agent_listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let hook_listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let agent_addr = agent_listener.local_addr().unwrap();
    let hook_addr = hook_listener.local_addr().unwrap().to_string();
    let tasks = vec![
        tokio::spawn(scheduler.run()),
        tokio::spawn(serve_agents(agent_listener, secret.clone(), agents)),
        tokio::spawn(serve_hooks(hook_listener, secret, hooks)),
    ];
    Hub {
        fake,
        hook_addr,
        agent_addr,
        control,
        actor: Some(tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx))),
        tasks,
        state: Some(state),
    }
}

impl Hub {
    /// Sends one hook event the way `cctg hook` does.
    async fn hook(&self, session: &str, event: HookEvent) {
        let post = HookPost::new(
            HOST.into(),
            session.into(),
            CWD.into(),
            String::new(),
            event,
        );
        let secret = Secret::parse(SECRET).unwrap();
        hook::post(
            &cctg::tls::HubAddr::plain(self.hook_addr.as_str()),
            &secret,
            &post,
            WAIT,
        )
        .await
        .expect("hub took the hook event");
    }

    async fn start(&self, session: &str, pid: u32) {
        self.hook(
            session,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
        .await;
    }

    async fn end(&self, session: &str, pid: u32) {
        self.hook(
            session,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(pid),
            },
        )
        .await;
    }

    async fn numbers(&self, session: &str) {
        self.hook(
            session,
            HookEvent::StatusLine {
                model: Some("Opus 5.5".into()),
                effort: Some("high".into()),
                context: Some(50),
                five_hour: Some(3),
                seven_day: Some(92),
            },
        )
        .await;
    }

    fn press(&self, message_id: i64, data: &str) {
        self.control
            .send(Control::Callback(CallbackInput {
                chat: Some(GROUP),
                query_id: format!("q-{data}"),
                data: Some(data.into()),
                message_id: Some(message_id),
                thread_id: None,
                from_name: None,
                display_name: None,
                sender: cctg::hub::chat::PrivateChat::of_user(1001),
            }))
            .unwrap();
    }

    /// Waits until `ready` holds for the ops made so far.
    async fn until(&self, what: &str, ready: impl Fn(&[Op]) -> bool) -> Vec<Op> {
        let reached = async {
            loop {
                let ops = self.fake.ops();
                if ready(&ops) {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, reached)
            .await
            .unwrap_or_else(|_| panic!("{what}: {:#?}", self.fake.ops()))
    }

    /// Waits until the status message of topic `thread` shows `want`.
    async fn shows(&self, what: &str, thread: i64, want: (String, Vec<String>)) {
        self.until(what, |ops| {
            current_status(ops, thread).is_some_and(|(_, shown)| shown == want)
        })
        .await;
    }

    /// The status message of topic `thread` now.
    fn status(&self, thread: i64) -> i64 {
        current_status(&self.fake.ops(), thread)
            .map(|(id, _)| id)
            .expect("a status message")
    }

    /// Waits until the slot actor has bound the agent of topic `thread`
    /// (its alive icon shows): the link answers `registered` before the
    /// actor takes the agent from its own channel, so a command or a key
    /// asked for right after `Agent::connect` can find no agent yet.
    async fn agent_bound(&self, thread: i64) {
        self.until("the agent bound", |ops| {
            topic_icon(ops, thread) == Some(ICON_ALIVE)
        })
        .await;
    }

    /// The status message of topic `thread`, once sent.
    async fn status_message(&self, thread: i64) -> i64 {
        let ops = self
            .until("status message sent", |ops| {
                current_status(ops, thread).is_some()
            })
            .await;
        assert!(
            !ops.iter().any(|op| matches!(
                op,
                Op::Send {
                    reply_markup: Some(_),
                    permission: false,
                    notify: true,
                    ..
                }
            )),
            "status messages go without a sound"
        );
        current_status(&ops, thread).map(|(id, _)| id).unwrap()
    }

    /// Stops the hub the way `cctg hub` does (the actor writes the registry)
    /// and starts a new one on the same state, with a fresh Telegram fake.
    async fn restart(mut self, every: Duration) -> Hub {
        self.control.send(Control::Stop).unwrap();
        let actor = self.actor.take().unwrap();
        tokio::time::timeout(WAIT, actor)
            .await
            .expect("the actor stopped")
            .unwrap();
        let state = self.state.take().unwrap();
        drop(self);
        start_hub_in(state, every).await
    }
}

// ---------------------------------------------------------------- agent

/// A session agent on a real link that presses keys and types commands
/// (`console_keys`, `console_commands`).
struct Agent {
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Agent {
    async fn connect(hub: &Hub, session: &str, pid: u32) -> Self {
        Self::connect_with(hub, session, pid, false).await
    }

    /// `status_lines`: the agent passes status line numbers on (TASK-058).
    async fn connect_with(hub: &Hub, session: &str, pid: u32, status_lines: bool) -> Self {
        let stream = TcpStream::connect(hub.agent_addr).await.unwrap();
        let (read, mut write) = stream.into_split();
        let hello = AgentMsg::Hello {
            secret: Secret::parse(SECRET).unwrap(),
        };
        wire::write_msg(&mut write, &hello).await.unwrap();
        let register = AgentMsg::Register(Register {
            session_id: session.into(),
            host: HOST.into(),
            cwd: CWD.into(),
            claude_pid: Some(pid),
            verdict_ack: true,
            transcript_reads: false,
            console_keys: true,
            console_commands: true,
            console_line_chars: 0,
            client: None,
            files: false,
            session_reads: false,
            status_lines,
            private_place: false,
            enrolled: None,
            heartbeat: false,
            sandbox: None,
        });
        wire::write_msg(&mut write, &register).await.unwrap();
        let mut agent = Self {
            reader: BufReader::new(read),
            write,
        };
        assert_eq!(
            agent.next().await,
            Some(HubMsg::Registered {
                files: true,
                heartbeat: true,
                albums: true,
            })
        );
        agent
    }

    /// The next hub message within `wait`, `None` when none came.
    async fn next_within(&mut self, wait: Duration) -> Option<HubMsg> {
        let mut line = Vec::new();
        match tokio::time::timeout(wait, wire::read_line(&mut self.reader, &mut line)).await {
            Ok(Ok(())) => Some(wire::decode(&line).unwrap()),
            _ => None,
        }
    }

    async fn next(&mut self) -> Option<HubMsg> {
        self.next_within(WAIT).await
    }

    /// No hub message in 500 ms.
    async fn quiet(&mut self) -> bool {
        self.next_within(Duration::from_millis(500)).await.is_none()
    }

    /// The next console key asked for: its id.
    async fn key(&mut self) -> u64 {
        match self.next().await {
            Some(HubMsg::ConsoleKey {
                key_id,
                key: ConsoleKey::Interrupt,
            }) => key_id,
            other => panic!("no console key: {other:?}"),
        }
    }

    async fn written(&mut self, key_id: u64, written: bool) {
        self.send(AgentMsg::ConsoleKeyWritten { key_id, written })
            .await;
    }

    async fn send(&mut self, msg: AgentMsg) {
        wire::write_msg(&mut self.write, &msg).await.unwrap();
    }
}

fn tool(id: &str, line: &str) -> HookEvent {
    HookEvent::ToolStart {
        tool_use_id: id.into(),
        line: line.into(),
    }
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_message_follows_the_session_and_its_button_writes_esc() {
    let hub = start_hub("follow", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect(&hub, A, 10).await;
    let status = hub.status_message(100).await;
    assert_eq!(
        status_sends(&hub.fake.ops()),
        [(100, "💤 Ждёт вас".to_owned())]
    );
    // The fake records a send before the hub has its answer: only an edit
    // of the message proves the hub knows it as the status message
    // (TASK-082; a notice before that is about no message of the hub).
    hub.numbers(A).await;
    hub.shows(
        "numbers shown",
        100,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;

    // The bot's pin notice about the status message goes (a hub before
    // TASK-062 pinned it); one about another message stays.
    hub.control
        .send(Control::Pinned {
            chat: GROUP,
            message_id: 5000,
            pinned: status,
        })
        .unwrap();
    hub.control
        .send(Control::Pinned {
            chat: GROUP,
            message_id: 5001,
            pinned: 777,
        })
        .unwrap();
    let ops = hub
        .until("pin notice deleted", |ops| {
            ops.iter().any(|op| {
                matches!(
                    op,
                    Op::Delete {
                        chat: GROUP,
                        message_id: 5000
                    }
                )
            })
        })
        .await;
    assert!(!ops.iter().any(|op| matches!(
        op,
        Op::Delete {
            chat: GROUP,
            message_id: 5001
        }
    )));

    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.hook(A, tool("t1", "• Bash: sleep 30")).await;
    hub.shows(
        "running call shown with ⏹",
        100,
        shown(&format!("⚙️ Bash: sleep 30\n{NUMBERS}"), &["status:stop"]),
    )
    .await;

    // ⏹ asks first and only the second press writes Esc.
    hub.press(status, "status:stop");
    hub.until("confirmation shown", |ops| {
        current_status(ops, 100).is_some_and(|(_, (_, buttons))| {
            buttons.first().map(String::as_str) == Some("status:confirm")
        })
    })
    .await;
    assert!(agent.quiet().await, "a first press sends nothing");
    hub.press(status, "status:confirm");
    let key_id = agent.key().await;
    agent.written(key_id, true).await;
    // Written is not stopped: the message says Esc was sent, without ⏹.
    hub.shows(
        "a written Esc is shown as sent",
        100,
        shown(&format!("⏹ Esc отправлен в терминал\n{NUMBERS}"), &[]),
    )
    .await;
    hub.press(status, "status:stop");
    hub.until("a press after Esc answered", |ops| answers(ops).len() >= 3)
        .await;
    assert!(agent.quiet().await, "no second Esc for the same turn");
    // The turn's real end.
    hub.hook(
        A,
        HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: None,
        },
    )
    .await;
    hub.shows(
        "Stop ends the turn",
        100,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    hub.press(status, "status:stop");
    let ops = hub
        .until("idle press answered", |ops| answers(ops).len() >= 4)
        .await;
    assert_eq!(
        answers(&ops),
        [
            status::ANSWER_CONFIRM,
            status::ANSWER_INTERRUPTING,
            status::ANSWER_IDLE,
            status::ANSWER_IDLE,
        ]
    );
    assert!(agent.quiet().await);
    assert_status_last(&ops, 100);
    assert_eq!(status_sends(&ops).len(), 1, "nothing came below it yet");

    // A key that was not written is told once in the topic; the notice
    // comes below the status message, which moves below it (TASK-062).
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.until("thinking", |ops| {
        current_status(ops, 100).is_some_and(|(_, (text, _))| text.starts_with("💭 Думает"))
    })
    .await;
    hub.press(status, "status:stop");
    hub.press(status, "status:confirm");
    let key_id = agent.key().await;
    agent.written(key_id, false).await;
    hub.until("failure told", |ops| key_failed_notices(ops) == 1)
        .await;
    let ops = hub
        .until("the status message moved below the notice", |ops| {
            status_sends(ops).len() == 2 && topic(ops, 100).len() == 2
        })
        .await;
    assert_status_last(&ops, 100);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Delete { message_id, .. } if *message_id == status)),
        "the old status message is deleted"
    );
    let moved = hub.status(100);
    assert_ne!(moved, status);

    // The end of the session: the message says so, without buttons, and a
    // press does nothing; a press on the old message is stale.
    hub.end(A, 10).await;
    hub.shows(
        "ended",
        100,
        shown(&format!("🏁 Сессия завершена\n{NUMBERS}"), &[]),
    )
    .await;
    hub.press(moved, "status:confirm");
    hub.until("dead press answered", |ops| {
        answers(ops).last().map(String::as_str) == Some(status::ANSWER_OFFLINE)
    })
    .await;
    hub.press(status, "status:confirm");
    hub.until("old press answered", |ops| {
        answers(ops).last().map(String::as_str) == Some(status::ANSWER_STALE)
    })
    .await;
    assert!(agent.quiet().await);
    assert_status_last(&hub.fake.ops(), 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_permission_prompt_hides_stop_and_a_press_writes_nothing() {
    let hub = start_hub("waiting", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect(&hub, A, 10).await;
    hub.status_message(100).await;
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.hook(A, tool("t1", "• Bash: rm build")).await;
    hub.shows(
        "a running call offers ⏹",
        100,
        shown("⚙️ Bash: rm build", &["status:stop"]),
    )
    .await;
    // Armed before the prompt opened.
    hub.press(hub.status(100), "status:stop");
    hub.until("armed", |ops| answers(ops).len() == 1).await;
    agent
        .send(AgentMsg::PermissionRequest(PermissionRequest {
            request_id: "qwert".into(),
            tool_name: "Bash".into(),
            description: "rm build".into(),
            input_preview: "rm -rf build".into(),
        }))
        .await;
    // The prompt comes below the status message, which moves below it.
    let ops = hub
        .until("the prompt", |ops| !prompts(ops).is_empty())
        .await;
    let (prompt, request_id) = prompts(&ops).pop().expect("prompt shown");
    let ops = hub
        .until("a waiting prompt hides ⏹ below the prompt", |ops| {
            current_status(ops, 100)
                .is_some_and(|(id, now)| id > prompt && now == shown("❓ Ждёт разрешения", &[]))
        })
        .await;
    assert_status_last(&ops, 100);
    // An old button (first or confirming press) writes nothing: Esc would
    // answer the prompt, not stop the turn.
    let status = hub.status(100);
    hub.press(status, "status:confirm");
    hub.press(status, "status:stop");
    let ops = hub
        .until("presses answered", |ops| answers(ops).len() == 3)
        .await;
    assert_eq!(
        answers(&ops)[1..],
        [status::ANSWER_WAITING, status::ANSWER_WAITING]
    );
    assert!(agent.quiet().await, "no Esc while the prompt waits");

    // The prompt is answered in Telegram; the call still runs: ⏹ is back and
    // a fresh confirmation writes Esc.
    hub.press(prompt, &format!("allow:{request_id}"));
    let verdict_id = match agent.next().await {
        Some(HubMsg::PermissionVerdict {
            verdict_id: Some(id),
            ..
        }) => id,
        other => panic!("no verdict: {other:?}"),
    };
    agent.send(AgentMsg::PermissionAck { verdict_id }).await;
    hub.shows(
        "⏹ back once the prompt is answered",
        100,
        shown("⚙️ Bash: rm build", &["status:stop"]),
    )
    .await;
    hub.press(status, "status:stop");
    hub.press(status, "status:confirm");
    agent.key().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_key_answer_never_reaches_the_next_session_of_the_slot() {
    let hub = start_hub("late", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent_a = Agent::connect(&hub, A, 10).await;
    let status = hub.status_message(100).await;
    hub.agent_bound(100).await;
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.until("A thinks", |ops| {
        current_status(ops, 100).is_some_and(|(_, (text, _))| text == "💭 Думает")
    })
    .await;
    hub.press(status, "status:stop");
    hub.press(status, "status:confirm");
    let key_id = agent_a.key().await;

    // A ends before its agent answers; B takes the same slot and topic: its
    // separator comes below A's status message, which moves below it.
    hub.end(A, 10).await;
    hub.start(B, 11).await;
    let _agent_b = Agent::connect(&hub, B, 11).await;
    hub.hook(B, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.shows(
        "B thinks in the slot's status message",
        100,
        shown("💭 Думает", &["status:stop"]),
    )
    .await;
    // A's agent (its link still open) answers now.
    agent_a.written(key_id, false).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ops = hub.fake.ops();
    assert_eq!(key_failed_notices(&ops), 0, "no notice in B's topic");
    assert_eq!(
        current_status(&ops, 100).map(|(_, shown)| shown),
        Some(shown("💭 Думает", &["status:stop"])),
        "B's turn is untouched"
    );
    assert_status_last(&ops, 100);
    assert_eq!(
        status_sends(&ops).len(),
        2,
        "one more message: below the separator"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restarted_hub_keeps_its_status_message_and_the_numbers() {
    let hub = start_hub("restart", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let _agent = Agent::connect(&hub, A, 10).await;
    let status = hub.status_message(100).await;
    hub.numbers(A).await;
    hub.shows(
        "numbers shown",
        100,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    let hub = hub.restart(Duration::from_millis(50)).await;
    // The same message is edited: no second message, and the numbers are
    // still there without a new status line call.
    hub.until("the same message after the restart", |ops| {
        edits(ops, status).last() == Some(&shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = hub.fake.ops();
    assert!(status_sends(&ops).is_empty(), "{ops:#?}");
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::Delete { .. } | Op::Unpin { .. })),
        "{ops:#?}"
    );
}

/// TASK-058: an agent that announces `status_lines` is told its session,
/// again after `/clear`, and the numbers it sends for that session show
/// like the hook's; numbers for another session are dropped. An agent
/// without it is never told.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numbers_over_the_agent_link_show_like_the_hooks_numbers() {
    let hub = start_hub("link-numbers", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect_with(&hub, A, 10, true).await;
    assert_eq!(
        agent.next().await,
        Some(HubMsg::Bound {
            session_id: A.into()
        })
    );
    let status = hub.status_message(100).await;
    // A live top-level session with its own status message, whose numbers
    // would show if A's link could send them.
    let other = "0a16e2e0-0000-4000-8000-000000000293";
    hub.start(other, 11).await;
    let other_status = hub.status_message(101).await;
    assert_ne!(status, other_status);
    let numbers = |session: &str, context| AgentMsg::StatusLine {
        session_id: session.into(),
        model: Some("Opus 5.5".into()),
        effort: Some("high".into()),
        context: Some(context),
        five_hour: Some(3),
        seven_day: Some(92),
    };
    // Another live session's numbers first: dropped.
    agent.send(numbers(other, 77)).await;
    agent.send(numbers(A, 50)).await;
    hub.shows(
        "numbers from the link shown",
        100,
        shown(
            &format!(
                "💤 Ждёт вас
{NUMBERS}"
            ),
            &[],
        ),
    )
    .await;
    // Frames of one link are handled in order and edits go every 50 ms.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !hub.fake.ops().iter().any(|op| matches!(
            op,
            Op::Send { text, .. } | Op::Edit { text, .. } if text.contains("ctx 77%")
        )),
        "numbers of another session shown"
    );
    // `/clear`: the same claude process runs a new session.
    hub.hook(
        A,
        HookEvent::SessionEnd {
            reason: Some("clear".into()),
            claude_pid: Some(10),
        },
    )
    .await;
    hub.hook(
        B,
        HookEvent::SessionStart {
            source: Some("clear".into()),
            claude_pid: Some(10),
            parent_claude_pid: None,
        },
    )
    .await;
    assert_eq!(
        agent.next().await,
        Some(HubMsg::Bound {
            session_id: B.into()
        })
    );
    agent.send(numbers(B, 60)).await;
    hub.until("the new session's numbers shown", |ops| {
        ops.iter().any(|op| {
            matches!(op, Op::Send { text, .. } | Op::Edit { text, .. } if text.contains("ctx 60%"))
        })
    })
    .await;
    // An agent built before TASK-058 is never told its session.
    let mut old = Agent::connect(&hub, other, 11).await;
    assert!(old.quiet().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_reaches_only_the_agent_of_its_own_slot() {
    let hub = start_hub("slots", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent_a = Agent::connect(&hub, A, 10).await;
    let status_a = hub.status_message(100).await;
    hub.start(B, 11).await;
    let mut agent_b = Agent::connect(&hub, B, 11).await;
    let status_b = hub.status_message(101).await;
    assert_ne!(status_a, status_b);
    for session in [A, B] {
        hub.hook(session, HookEvent::UserPromptSubmit { prompt_id: None })
            .await;
    }
    hub.until("both think", |ops| {
        [status_a, status_b].iter().all(|status| {
            edits(ops, *status)
                .last()
                .is_some_and(|(text, _)| text.starts_with("💭"))
        })
    })
    .await;
    // A running call offers ⏹ only.
    hub.hook(A, tool("t1", "• Bash: sleep 30")).await;
    hub.shows(
        "A runs its call with ⏹ only",
        100,
        shown("⚙️ Bash: sleep 30", &["status:stop"]),
    )
    .await;
    // Armed on A's message; B's message is not armed by it.
    hub.press(status_a, "status:stop");
    hub.press(status_b, "status:confirm");
    hub.until("B asked for its own confirmation", |ops| {
        answers(ops).len() >= 2
    })
    .await;
    assert!(agent_a.quiet().await);
    assert!(agent_b.quiet().await);
    hub.press(status_a, "status:confirm");
    agent_a.key().await;
    assert!(agent_b.quiet().await);
    // A button of a message that is no status message does nothing.
    hub.press(4242, "status:confirm");
    hub.until("stale press answered", |ops| {
        answers(ops).last().map(String::as_str) == Some(status::ANSWER_STALE)
    })
    .await;
    assert!(agent_a.quiet().await && agent_b.quiet().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_are_paced_and_a_deleted_status_message_comes_back() {
    let hub = start_hub("pace", Duration::from_millis(400)).await;
    hub.start(A, 10).await;
    let _agent = Agent::connect(&hub, A, 10).await;
    let status = hub.status_message(100).await;
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    let started = std::time::Instant::now();
    // About 1.2 s of calls and status lines, one every 30 ms: three pacing
    // windows.
    for index in 0..20 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        hub.hook(A, tool(&format!("t{index}"), &format!("• Read: f{index}")))
            .await;
        hub.hook(
            A,
            HookEvent::StatusLine {
                model: None,
                effort: None,
                context: Some(index),
                five_hour: None,
                seven_day: None,
            },
        )
        .await;
        hub.hook(
            A,
            HookEvent::ToolEnd {
                tool_use_id: format!("t{index}"),
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    hub.hook(A, tool("last", "• Bash: build")).await;
    let ops = hub
        .until("the last state shown", |ops| {
            edits(ops, status)
                .last()
                .is_some_and(|(text, _)| text == "⚙️ Bash: build\nctx 19%")
        })
        .await;
    let windows = started.elapsed().as_millis() / 400 + 2;
    assert!(
        edits(&ops, status).len() as u128 <= windows,
        "{} edits in {:?}",
        edits(&ops, status).len(),
        started.elapsed()
    );
    // Deleted in Telegram: a new message is sent.
    hub.fake
        .edit_errors
        .lock()
        .unwrap()
        .push_back("Bad Request: message to edit not found");
    hub.hook(
        A,
        HookEvent::ToolEnd {
            tool_use_id: "last".into(),
        },
    )
    .await;
    let ops = hub
        .until("a second status message", |ops| {
            status_sends(ops).len() == 2
        })
        .await;
    assert_eq!(
        status_sends(&ops)[1],
        (100, "💭 Думает\nctx 19%".to_owned())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_console_command_goes_over_the_link_and_its_answer_comes_back() {
    let hub = start_hub("console", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect(&hub, A, 10).await;
    hub.status_message(100).await;
    hub.agent_bound(100).await;
    let say = |message_id: i64, text: &str| {
        hub.control
            .send(Control::Message(Inbound {
                display_name: None,
                chat: GROUP,
                sender: cctg::hub::chat::PrivateChat::of_user(1001),
                message_id,
                thread_id: Some(100),
                text: Some(text.into()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
                from_name: None,
                from_username: false,
                author: None,
                reply_from: None,
            }))
            .unwrap();
    };
    let mut typed = Vec::new();
    for (message_id, text) in [(41, "!echo hi"), (42, "/compact")] {
        say(message_id, text);
        match agent.next().await {
            Some(HubMsg::ConsoleCommand {
                command_id,
                text: got,
            }) => {
                assert_eq!(got, text);
                typed.push(command_id);
            }
            other => panic!("no console command: {other:?}: {:#?}", hub.fake.ops()),
        }
    }
    let answer = |command_id, outcome| AgentMsg::ConsoleCommandTyped {
        command_id,
        outcome,
        panel: None,
    };
    agent.send(answer(typed[0], CommandOutcome::Sent)).await;
    agent.send(answer(typed[1], CommandOutcome::Draft)).await;
    let ops = hub
        .until("both answers handled", |ops| {
            let reacted = ops.iter().any(|op| {
                matches!(op, Op::React { chat: GROUP, message_id: 41, emoji } if emoji == stream::ACCEPTED)
            });
            let told = ops.iter().any(|op| {
                matches!(op, Op::Send { reply_to: Some(42), text, .. } if text == console::DRAFT_NOTICE)
            });
            reacted && told
        })
        .await;
    assert!(
        !ops.iter().any(|op| matches!(
            op,
            Op::Send {
                reply_to: Some(41),
                ..
            }
        )),
        "a typed command gets no text answer"
    );
    assert!(agent.quiet().await, "nothing went to the model");
}

/// TASK-087: the agent of a sandboxed folder answers `refused` over the real
/// link (the agent side: `agent.rs` tests); the topic gets the neutral
/// notice and the model nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_console_command_is_answered_over_the_link() {
    let hub = start_hub("console-refused", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect(&hub, A, 10).await;
    hub.status_message(100).await;
    hub.agent_bound(100).await;
    hub.control
        .send(Control::Message(Inbound {
            display_name: None,
            chat: GROUP,
            sender: cctg::hub::chat::PrivateChat::of_user(1001),
            message_id: 51,
            thread_id: Some(100),
            text: Some("!cat ~/marker".into()),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
            from_username: false,
            author: None,
            reply_from: None,
        }))
        .unwrap();
    let command_id = match agent.next().await {
        Some(HubMsg::ConsoleCommand { command_id, .. }) => command_id,
        other => panic!("no console command: {other:?}"),
    };
    agent
        .send(AgentMsg::ConsoleCommandTyped {
            command_id,
            outcome: CommandOutcome::Refused,
            panel: None,
        })
        .await;
    hub.until("the refusal answered", |ops| {
        ops.iter().any(|op| {
            matches!(op, Op::Send { reply_to: Some(51), text, .. } if text == console::REFUSED_NOTICE)
        })
    })
    .await;
    assert!(agent.quiet().await, "nothing went to the model");
}

/// A home directory whose `.cctg/device.env` points at `addr`.
fn hook_home(test: &str, addr: &str) -> PathBuf {
    let home = common::own_tmp().join(format!("status-e2e-{test}"));
    let dir = home.join(".cctg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("device.env"),
        format!("CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST={HOST}\n"),
    )
    .unwrap();
    home
}

/// What the user typed after `/compact`; it must never leave the hook.
const COMPACT_FOCUS: &str = "private compact focus";

/// Runs the real `cctg hook PreCompact` of session A.
async fn pre_compact(home: &Path, trigger: &str) -> Output {
    let input = serde_json::json!({
        "session_id": A,
        "transcript_path": "/p/a.jsonl",
        "cwd": CWD,
        "hook_event_name": "PreCompact",
        "trigger": trigger,
        "custom_instructions": (trigger == "manual").then_some(COMPACT_FOCUS),
    })
    .to_string();
    let home = home.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut child = common::spawn(
            common::cctg(&home)
                .args(["hook", "PreCompact"])
                .env("RUST_LOG", "trace")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .expect("cctg starts");
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input.as_bytes());
        drop(stdin);
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    // Exit 0 and no stdout: the hook never blocks the compaction.
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(COMPACT_FOCUS), "{stderr}");
    assert!(!stderr.contains(SECRET), "{stderr}");
    assert!(!stderr.contains("not delivered"), "{stderr}");
    output
}

/// Silent topic lines about compactions, in send order.
fn compact_lines(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Send {
                thread_id: Some(100),
                text,
                reply_markup: None,
                notify: false,
                ..
            } if text.starts_with("🗜") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_from_the_real_hook_shows_in_the_status_and_the_topic() {
    let hub = start_hub("compact", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    hub.status_message(100).await;
    hub.numbers(A).await;
    hub.shows(
        "numbers shown",
        100,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    let home = hook_home("compact", &hub.hook_addr);

    // /compact with the user's text: status and one line, no text.
    pre_compact(&home, "manual").await;
    hub.shows(
        "manual compaction in the status",
        100,
        shown(&format!("🗜 Сжимаю контекст (вручную)…\n{NUMBERS}"), &[]),
    )
    .await;
    hub.until("its line", |ops| compact_lines(ops).len() == 1)
        .await;
    // Done: the status comes back; the line waits for the new percentage.
    hub.hook(
        A,
        HookEvent::SessionStart {
            source: Some("compact".into()),
            claude_pid: Some(10),
            parent_claude_pid: None,
        },
    )
    .await;
    hub.shows(
        "status back after the compaction",
        100,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    hub.hook(
        A,
        HookEvent::StatusLine {
            model: Some("Opus 5.5".into()),
            effort: Some("high".into()),
            context: Some(12),
            five_hour: Some(3),
            seven_day: Some(92),
        },
    )
    .await;
    let ops = hub
        .until("the done line", |ops| compact_lines(ops).len() == 2)
        .await;
    let lines = compact_lines(&ops);
    assert_eq!(lines[0], "🗜 Сжимаю контекст (вручную)…");
    assert!(
        lines[1].starts_with("🗜 Контекст сжат за ") && lines[1].ends_with(" с: 50% → 12%"),
        "{}",
        lines[1]
    );

    // Auto, cut by the session's end: no line of success.
    pre_compact(&home, "auto").await;
    hub.until("auto compaction in the status", |ops| {
        current_status(ops, 100)
            .is_some_and(|(_, (text, _))| text.starts_with("🗜 Сжимаю контекст (авто)…"))
    })
    .await;
    hub.end(A, 10).await;
    hub.until("ended", |ops| {
        current_status(ops, 100)
            .is_some_and(|(_, (text, _))| text.starts_with("🏁 Сессия завершена"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = hub.fake.ops();
    assert_eq!(
        compact_lines(&ops),
        [
            lines[0].clone(),
            lines[1].clone(),
            "🗜 Сжимаю контекст (авто)…".to_owned()
        ]
    );
    let everything = format!("{ops:?}");
    assert!(!everything.contains(COMPACT_FOCUS));
}

// ---------------------------------------------------------------- TASK-088

/// What the relay does to the first hook request it takes; later ones pass
/// untouched.
#[derive(Clone, Copy)]
enum Fault {
    /// Reads the request and never forwards or answers it.
    Hold,
    /// Forwards the request and holds the hub's answer for 2 s.
    LateAnswer,
}

/// A loopback relay in front of the hub's hook endpoint that counts its
/// connections.
struct Relay {
    addr: String,
    connections: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay(to: String, fault: Fault) -> Relay {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = connections.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            let to = to.clone();
            tokio::spawn(async move {
                if !first {
                    if let Ok(mut hub) = TcpStream::connect(&to).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut hub).await;
                    }
                    return;
                }
                match fault {
                    Fault::Hold => {
                        let mut buf = [0u8; 4096];
                        while matches!(client.read(&mut buf).await, Ok(n) if n > 0) {}
                    }
                    Fault::LateAnswer => {
                        let Ok(hub) = TcpStream::connect(&to).await else {
                            return;
                        };
                        let (mut from_client, mut to_client) = client.into_split();
                        let (mut from_hub, mut to_hub) = hub.into_split();
                        let up = tokio::spawn(async move {
                            let _ = tokio::io::copy(&mut from_client, &mut to_hub).await;
                        });
                        // The hub's status line and head.
                        let mut answer = Vec::new();
                        let mut buf = [0u8; 1024];
                        let read = async {
                            while !answer.windows(4).any(|w| w == b"\r\n\r\n") {
                                match from_hub.read(&mut buf).await {
                                    Ok(n) if n > 0 => answer.extend_from_slice(&buf[..n]),
                                    _ => break,
                                }
                            }
                        };
                        let _ = tokio::time::timeout(WAIT, read).await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        let _ = to_client.write_all(&answer).await;
                        let _ = to_client.shutdown().await;
                        up.abort();
                    }
                }
            });
        }
    });
    Relay {
        addr,
        connections,
        task,
    }
}

/// The session's agent: the real link code in process, its spool replays
/// going through `relay`. Its link events are drained once it is up.
async fn replaying_agent(
    hub: &Hub,
    home: &Path,
    relay: &Relay,
) -> (mpsc::Sender<AgentMsg>, JoinHandle<()>) {
    use cctg::agent::{self, Backoff, LinkConfig, LinkEvent, Replay, StatusWatch};
    let state = home.join(".cctg");
    let (outbox, mut events) = agent::spawn(LinkConfig {
        addr: cctg::tls::HubAddr::plain(hub.agent_addr.to_string()),
        secret: Secret::parse(SECRET).unwrap(),
        register: Register {
            session_id: A.into(),
            host: HOST.into(),
            cwd: CWD.into(),
            claude_pid: Some(10),
            verdict_ack: true,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            console_line_chars: 0,
            client: None,
            files: false,
            session_reads: false,
            status_lines: true,
            private_place: false,
            enrolled: None,
            heartbeat: false,
            sandbox: None,
        },
        backoff: Backoff::default(),
        replay: Some(Replay {
            spool: cctg::spool::dir(&state),
            hook_addr: cctg::tls::HubAddr::plain(relay.addr.as_str()),
        }),
        heartbeat: Default::default(),
        status: Some(StatusWatch::new(state)),
        sandbox: None,
    });
    let up = async {
        loop {
            match events.recv().await {
                Some(LinkEvent::Up { .. }) => return,
                Some(_) => continue,
                None => panic!("link stopped"),
            }
        }
    };
    tokio::time::timeout(WAIT, up).await.expect("agent up");
    // An undrained receiver would block the link.
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
    (outbox, drain)
}

const ANSWER: &str = "answer of turn one";

/// Runs the real `cctg hook Stop` of session A with [`ANSWER`]; its stderr.
async fn stop_hook(home: &Path) -> String {
    let input = serde_json::json!({
        "session_id": A,
        "transcript_path": "/p/a.jsonl",
        "cwd": CWD,
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": ANSWER,
    })
    .to_string();
    let home = home.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut child = common::spawn(
            common::cctg(&home)
                .args(["hook", "Stop"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .expect("cctg starts");
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input.as_bytes());
        drop(stdin);
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stderr.contains(SECRET) && !stderr.contains(ANSWER),
        "{stderr}"
    );
    stderr
}

/// Sends of [`ANSWER`] into topic `thread`.
fn answer_sends(ops: &[Op], thread: i64) -> usize {
    ops.iter()
        .filter(|op| {
            matches!(op, Op::Send { thread_id: Some(t), text, .. } if *t == thread && text.contains(ANSWER))
        })
        .count()
}

/// Waits until the spool of `home` holds no file of session A.
async fn spool_emptied(home: &Path) {
    let dir = cctg::spool::dir(&home.join(".cctg")).join(A);
    let deadline = tokio::time::Instant::now() + WAIT;
    while std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some()) {
        assert!(tokio::time::Instant::now() < deadline, "the spool emptied");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// TASK-088 acceptance: the hub does not answer the `Stop` hook in time (the
/// request never reaches it); the hook keeps the event and the session's
/// agent delivers it, with no other hook: the answer reaches the topic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_the_hub_did_not_answer_in_time_still_reaches_the_topic() {
    let hub = start_hub("stop-held", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let relay = relay(hub.hook_addr.clone(), Fault::Hold).await;
    let home = hook_home("stop-held", &relay.addr);
    let (_outbox, _drain) = replaying_agent(&hub, &home, &relay).await;
    hub.agent_bound(100).await;

    let stderr = stop_hook(&home).await;
    assert!(stderr.contains("kept for the next hook"), "{stderr}");
    hub.until("the answer in the topic", |ops| answer_sends(ops, 100) == 1)
        .await;
    spool_emptied(&home).await;
}

/// TASK-088: the hub took the `Stop` but its answer came after the hook's
/// budget; the agent sends the kept copy and the hub drops it as a repeat:
/// the answer shows exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_the_hub_took_but_answered_late_shows_once() {
    let hub = start_hub("stop-late", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let relay = relay(hub.hook_addr.clone(), Fault::LateAnswer).await;
    let home = hook_home("stop-late", &relay.addr);
    let (_outbox, _drain) = replaying_agent(&hub, &home, &relay).await;
    hub.agent_bound(100).await;

    let stderr = stop_hook(&home).await;
    assert!(stderr.contains("kept for the next hook"), "{stderr}");
    hub.until("the answer in the topic", |ops| answer_sends(ops, 100) >= 1)
        .await;
    spool_emptied(&home).await;
    // A negative check: the replayed copy never shows.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(answer_sends(&hub.fake.ops(), 100), 1);
    assert_eq!(relay.connections.load(Ordering::SeqCst), 2);
}

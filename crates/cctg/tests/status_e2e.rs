//! Status message end to end (TASK-029): hook events over real HTTP to the
//! real `serve_hooks`, an agent over a real TCP link to the real
//! `serve_agents` (a hand-written client that announces `console_keys` and
//! answers `console_key`), the real `Slots` actor and `Scheduler`, and a fake
//! Telegram transport. Button presses come in as the update poll hands them
//! over (`Control::Callback`); which updates get that far (allowlist, the
//! bot's own pin notices) is tested in `hub::updates` and `hub::mod`.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hook;
use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::ingress::{bind, serve_agents, serve_hooks};
use cctg::hub::registry::RegistryStore;
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

fn pins(ops: &[Op]) -> Vec<i64> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Pin { message_id } => Some(*message_id),
            _ => None,
        })
        .collect()
}

/// Edits of `message`: text and the callback data of its buttons.
fn edits(ops: &[Op], message: i64) -> Vec<(String, Vec<String>)> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Edit {
                message_id,
                text,
                reply_markup,
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
    let registry = store.load().unwrap();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(every),
        ..Options::default()
    };
    let (slots, _view) = Slots::new(registry, store, outbox, options);
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
        hook::post(&self.hook_addr, &secret, &post, WAIT)
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
                query_id: format!("q-{data}"),
                data: Some(data.into()),
                message_id: Some(message_id),
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

    /// Waits until the last edit of `status` is `want`.
    async fn shows(&self, what: &str, status: i64, want: (String, Vec<String>)) {
        self.until(what, |ops| edits(ops, status).last() == Some(&want))
            .await;
    }

    /// The status message of topic `thread`, once pinned.
    async fn status_message(&self, thread: i64) -> i64 {
        let ops = self
            .until("status message sent and pinned", |ops| {
                status_sends(ops).iter().any(|(t, _)| *t == thread) && !pins(ops).is_empty()
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
        // Sends are numbered from 1000 in call order.
        let index = ops
            .iter()
            .filter(|op| matches!(op, Op::Send { .. }))
            .position(|op| {
                matches!(op, Op::Send { thread_id: Some(t), reply_markup: Some(_), permission: false, .. } if *t == thread)
            })
            .unwrap();
        1000 + index as i64
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
            client: None,
        });
        wire::write_msg(&mut write, &register).await.unwrap();
        let mut agent = Self {
            reader: BufReader::new(read),
            write,
        };
        assert_eq!(agent.next().await, Some(HubMsg::Registered));
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
    assert_eq!(pins(&hub.fake.ops()), [status]);

    // The bot's pin notice about the status message goes; one about
    // another message stays.
    hub.control
        .send(Control::Pinned {
            message_id: 5000,
            pinned: status,
        })
        .unwrap();
    hub.control
        .send(Control::Pinned {
            message_id: 5001,
            pinned: 777,
        })
        .unwrap();
    let ops = hub
        .until("pin notice deleted", |ops| {
            ops.iter()
                .any(|op| matches!(op, Op::Delete { message_id: 5000 }))
        })
        .await;
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::Delete { message_id: 5001 }))
    );

    hub.numbers(A).await;
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.hook(A, tool("t1", "• Bash: sleep 30")).await;
    hub.shows(
        "running call shown with ⏹",
        status,
        shown(&format!("⚙️ Bash: sleep 30\n{NUMBERS}"), &["status:stop"]),
    )
    .await;

    // ⏹ asks first and only the second press writes Esc.
    hub.press(status, "status:stop");
    hub.until("confirmation shown", |ops| {
        edits(ops, status).last().is_some_and(|(_, buttons)| {
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
        status,
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
        status,
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

    // A key that was not written is told once in the topic.
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.until("thinking", |ops| {
        edits(ops, status)
            .last()
            .is_some_and(|(text, _)| text.starts_with("💭 Думает"))
    })
    .await;
    hub.press(status, "status:stop");
    hub.press(status, "status:confirm");
    let key_id = agent.key().await;
    agent.written(key_id, false).await;
    hub.until("failure told", |ops| key_failed_notices(ops) == 1)
        .await;

    // The end of the session: the message says so, without buttons, and a
    // press does nothing.
    hub.end(A, 10).await;
    hub.shows(
        "ended",
        status,
        shown(&format!("🏁 Сессия завершена\n{NUMBERS}"), &[]),
    )
    .await;
    hub.press(status, "status:confirm");
    hub.until("dead press answered", |ops| {
        answers(ops).last().map(String::as_str) == Some(status::ANSWER_OFFLINE)
    })
    .await;
    assert!(agent.quiet().await);
    assert_eq!(pins(&hub.fake.ops()).len(), 1, "pinned once");
    assert_eq!(
        status_sends(&hub.fake.ops()).len(),
        1,
        "one message per slot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_permission_prompt_hides_stop_and_a_press_writes_nothing() {
    let hub = start_hub("waiting", Duration::from_millis(50)).await;
    hub.start(A, 10).await;
    let mut agent = Agent::connect(&hub, A, 10).await;
    let status = hub.status_message(100).await;
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.hook(A, tool("t1", "• Bash: rm build")).await;
    hub.shows(
        "a running call offers ⏹",
        status,
        shown("⚙️ Bash: rm build", &["status:stop"]),
    )
    .await;
    // Armed before the prompt opened.
    hub.press(status, "status:stop");
    hub.until("armed", |ops| answers(ops).len() == 1).await;
    agent
        .send(AgentMsg::PermissionRequest(PermissionRequest {
            request_id: "qwert".into(),
            tool_name: "Bash".into(),
            description: "rm build".into(),
            input_preview: "rm -rf build".into(),
        }))
        .await;
    hub.shows(
        "a waiting prompt hides ⏹",
        status,
        shown("❓ Ждёт разрешения", &[]),
    )
    .await;
    // An old button (first or confirming press) writes nothing: Esc would
    // answer the prompt, not stop the turn.
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
    let (prompt, request_id) = prompts(&hub.fake.ops()).pop().expect("prompt shown");
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
        status,
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
    hub.hook(A, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.until("A thinks", |ops| {
        edits(ops, status)
            .last()
            .is_some_and(|(text, _)| text == "💭 Думает")
    })
    .await;
    hub.press(status, "status:stop");
    hub.press(status, "status:confirm");
    let key_id = agent_a.key().await;

    // A ends before its agent answers; B takes the same slot and topic.
    hub.end(A, 10).await;
    hub.start(B, 11).await;
    let _agent_b = Agent::connect(&hub, B, 11).await;
    hub.hook(B, HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.shows(
        "B thinks in the same status message",
        status,
        shown("💭 Думает", &["status:stop"]),
    )
    .await;
    // A's agent (its link still open) answers now.
    agent_a.written(key_id, false).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ops = hub.fake.ops();
    assert_eq!(key_failed_notices(&ops), 0, "no notice in B's topic");
    assert_eq!(
        edits(&ops, status).last(),
        Some(&shown("💭 Думает", &["status:stop"])),
        "B's turn is untouched"
    );
    assert_eq!(status_sends(&ops).len(), 1, "one message per slot");
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
        status,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    let hub = hub.restart(Duration::from_millis(50)).await;
    // The same message is edited: no second message, no second pin, and the
    // numbers are still there without a new status line call.
    hub.shows(
        "the same message after the restart",
        status,
        shown(&format!("💤 Ждёт вас\n{NUMBERS}"), &[]),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = hub.fake.ops();
    assert!(status_sends(&ops).is_empty(), "{ops:#?}");
    assert!(pins(&ops).is_empty(), "{ops:#?}");
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
        status_a,
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
    // Deleted in Telegram: a new message is sent and pinned.
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
            status_sends(ops).len() == 2 && pins(ops).len() == 2
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
    let say = |message_id: i64, text: &str| {
        hub.control
            .send(Control::Message(Inbound {
                message_id,
                thread_id: Some(100),
                text: Some(text.into()),
                reply_to: None,
                quote: None,
                forwarded: false,
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
            other => panic!("no console command: {other:?}"),
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
                matches!(op, Op::React { message_id: 41, emoji } if emoji == stream::ACCEPTED)
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

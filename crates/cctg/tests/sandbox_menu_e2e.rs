//! The sandbox switch of the menu end to end (TASK-090): presses as raw
//! `getUpdates` batches through the real update poll, the real `Slots`
//! actor, agents over real TCP links to the real `serve_agents` (the test
//! plays them with raw wire frames) and a fake Bot API.
//!
//! The owner turns the folder of a session of device `h1` into the sandbox
//! and back; each time every live session of `h1` that can take it gets a
//! quiet restart asked (the agent decides by folder and mode, here the test
//! plays its answer), the pressed session restarts and its next agent comes
//! with the new mode, which the row then shows. An old agent and a session
//! of another device are never asked; a refusal shows its reason under the
//! row. No topic ever shows a word of it.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::config::Allowlist;
use cctg::hub::groups::KnownGroups;
use cctg::hub::ingress::{bind, serve_agents};
use cctg::hub::offset::OffsetStore;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{Control, Options, Owners, Slots};
use cctg::hub::updates::{self, Routed, UpdateSource};
use cctg::wire::{
    self, AgentMsg, Client, HookEvent, HookPost, HubMsg, Register, SandboxOutcome, SandboxState,
    Secret, UpdateOutcome,
};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const GROUP_ID: i64 = -1001;
const SECRET: &str = "e2e-secret-0123456789abcdef";
const OWNER: i64 = 7_319_402_518;
const WAIT: Duration = Duration::from_secs(30);
/// How long the test listens for something that must not come.
const QUIET: Duration = Duration::from_millis(500);
const FOLDER: &str = "/w/proj";

/// The sessions: (session id, host, folder, claude pid, can switch).
const PRESSED: (&str, &str, &str, u32, bool) = (
    "5b090e2e-0000-4000-8000-000000000001",
    "h1",
    "/w/proj",
    4201,
    true,
);
const INSIDE: (&str, &str, &str, u32, bool) = (
    "5b090e2e-0000-4000-8000-000000000002",
    "h1",
    "/w/proj/sub",
    4202,
    true,
);
const NEIGHBOUR: (&str, &str, &str, u32, bool) = (
    "5b090e2e-0000-4000-8000-000000000003",
    "h1",
    "/w/projx",
    4203,
    true,
);
const OLD: (&str, &str, &str, u32, bool) = (
    "5b090e2e-0000-4000-8000-000000000004",
    "h1",
    "/w/old",
    4204,
    false,
);
const ELSEWHERE: (&str, &str, &str, u32, bool) = (
    "5b090e2e-0000-4000-8000-000000000005",
    "h2",
    "/w/proj",
    4205,
    true,
);

type Session = (&'static str, &'static str, &'static str, u32, bool);

fn owner_chat() -> Chat {
    Chat::Private(PrivateChat::of_user(OWNER))
}

fn mode(active: bool, wanted: bool) -> SandboxState {
    SandboxState {
        active,
        wanted,
        inherited: false,
    }
}

// ---------------------------------------------------------------- fake Telegram

#[derive(Debug, Clone)]
struct Shown {
    chat: Chat,
    thread: Option<i64>,
    id: i64,
    text: String,
    /// Its buttons: (text, callback data).
    buttons: Vec<(String, String)>,
}

#[derive(Default)]
struct Telegram {
    shown: Mutex<Vec<Shown>>,
    ops: Mutex<Vec<Op>>,
    next_message: Mutex<HashMap<Chat, i64>>,
    next_topic: Mutex<HashMap<Chat, i64>>,
}

fn buttons(markup: Option<&Value>) -> Vec<(String, String)> {
    markup
        .and_then(|markup| markup["inline_keyboard"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|button| {
            Some((
                button["text"].as_str()?.to_owned(),
                button["callback_data"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

impl Transport for Telegram {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        let Some(chat) = op.chat() else {
            return Ok(Outcome::Done);
        };
        let new_id = || {
            let mut next = self.next_message.lock().unwrap();
            let id = next.entry(chat).or_insert(1000);
            *id += 1;
            *id
        };
        let sent = |id| {
            Ok(Outcome::Sent(Message {
                message_id: id,
                ..Message::default()
            }))
        };
        match op {
            Op::CreateTopic { name, .. } => {
                let mut next = self.next_topic.lock().unwrap();
                let id = next.entry(chat).or_insert(100);
                *id += 1;
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: *id,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send {
                thread_id,
                text,
                reply_markup,
                ..
            } => {
                let id = new_id();
                self.shown.lock().unwrap().push(Shown {
                    chat,
                    thread: *thread_id,
                    id,
                    text: text.clone(),
                    buttons: buttons(reply_markup.as_ref()),
                });
                sent(id)
            }
            Op::Stream {
                thread_id,
                text,
                into: None,
                ..
            } => {
                let id = new_id();
                self.shown.lock().unwrap().push(Shown {
                    chat,
                    thread: Some(*thread_id),
                    id,
                    text: text.clone(),
                    buttons: Vec::new(),
                });
                sent(id)
            }
            Op::Stream { into: Some(id), .. } => sent(*id),
            Op::Edit {
                message_id,
                text,
                reply_markup,
                ..
            } => {
                let mut all = self.shown.lock().unwrap();
                let Some(shown) = all
                    .iter_mut()
                    .find(|shown| shown.chat == chat && shown.id == *message_id)
                else {
                    return Err(ApiError::Telegram {
                        code: 400,
                        description: "Bad Request: message to edit not found".into(),
                    });
                };
                shown.text.clone_from(text);
                if reply_markup.is_some() {
                    shown.buttons = buttons(reply_markup.as_ref());
                }
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Telegram {
    /// The owner's menu: the General message with the sessions tab.
    fn menu(&self) -> Option<Shown> {
        self.shown
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|shown| {
                shown.chat == owner_chat()
                    && shown.thread.is_none()
                    && shown.buttons.iter().any(|(_, data)| data == "menu:up:0")
            })
            .cloned()
    }

    /// The text the press `query` was answered with, once answered.
    fn answer(&self, query: &str) -> Option<Option<String>> {
        self.ops.lock().unwrap().iter().find_map(|op| match op {
            Op::AnswerCallback { query_id, text } if query_id == query => Some(text.clone()),
            _ => None,
        })
    }

    /// Everything handed to Telegram for a topic, as text: sends and
    /// streams into a topic, topic edits and edits of messages outside
    /// the General.
    fn topic_ops(&self) -> Vec<String> {
        let general: Vec<(Chat, i64)> = self
            .shown
            .lock()
            .unwrap()
            .iter()
            .filter(|shown| shown.thread.is_none())
            .map(|shown| (shown.chat, shown.id))
            .collect();
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|op| match op {
                Op::AnswerCallback { .. } | Op::Pin { .. } | Op::Unpin { .. } => false,
                Op::Send {
                    thread_id: None, ..
                } => false,
                Op::Edit {
                    chat, message_id, ..
                } => !general.contains(&(*chat, *message_id)),
                _ => true,
            })
            .map(|op| format!("{op:?}"))
            .collect()
    }
}

// ---------------------------------------------------------------- updates

/// `getUpdates`: the batches the test hands in, one per call.
struct Batches(tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<Value>>>);

impl UpdateSource for Batches {
    async fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Value>, ApiError> {
        match self.0.lock().await.recv().await {
            Some(batch) => Ok(batch),
            None => std::future::pending().await,
        }
    }
}

/// The owner's press of `data` on the General message `message_id`.
fn press(query: &str, message_id: i64, data: &str) -> Value {
    let from = json!({ "id": OWNER, "is_bot": false, "first_name": "Анна", "username": "anna" });
    json!({ "callback_query": {
        "id": query, "from": from, "chat_instance": "c", "data": data,
        "message": {
            "message_id": message_id, "date": 1,
            "chat": { "id": OWNER, "type": "private" },
        },
    }})
}

// ---------------------------------------------------------------- hub

struct Hub {
    telegram: Arc<Telegram>,
    batches: mpsc::UnboundedSender<Vec<Value>>,
    next_update: Mutex<i64>,
    next_query: Mutex<u32>,
    agent_addr: SocketAddr,
    hooks: mpsc::Sender<HookPost>,
    tasks: Vec<JoinHandle<()>>,
    state: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

async fn start_hub() -> Hub {
    let state = std::env::temp_dir().join(format!("cctg-sandbox-menu-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let telegram = Arc::new(Telegram::default());
    let fast = Limits::from(BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    });
    let outbox = Outbox::per_chat(telegram.clone(), fast, fast);
    let store = RegistryStore::open(&state).unwrap();
    let registry = store.load(GroupChat::of(GROUP_ID)).unwrap();
    let groups = KnownGroups::default();
    let allowlist: Allowlist = [OWNER].into_iter().collect();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(Duration::from_millis(50)),
        owners: Some(Owners {
            first: PrivateChat::of_user(OWNER),
            devices: None,
            share_new: false,
        }),
        menu: true,
        groups: groups.clone(),
        allowlist: allowlist.clone(),
        ..Options::default()
    };
    let slots = Slots::new(registry, store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let agent_addr = listener.local_addr().unwrap();
    let (batches, batches_rx) = mpsc::unbounded_channel();
    let offsets = OffsetStore::open(&state).unwrap();
    let poll = tokio::spawn(async move {
        let source = Batches(tokio::sync::Mutex::new(batches_rx));
        let route = move |routed| {
            let _ = match routed {
                Routed::Input(input) => control.send(Control::Message(input)),
                Routed::Callback(input) => control.send(Control::Callback(input)),
                _ => Ok(()),
            };
        };
        updates::poll_until(
            &source,
            &groups,
            &allowlist,
            true,
            &offsets,
            route,
            std::future::pending(),
        )
        .await;
    });
    let tasks = vec![
        tokio::spawn(serve_agents(
            listener,
            Secret::parse(SECRET).unwrap(),
            agents,
        )),
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)),
        poll,
    ];
    Hub {
        telegram,
        batches,
        next_update: Mutex::new(0),
        next_query: Mutex::new(0),
        agent_addr,
        hooks,
        tasks,
        state,
    }
}

impl Hub {
    async fn until(&self, what: &str, ready: impl Fn(&Telegram) -> bool) {
        let reached = async {
            while !ready(&self.telegram) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(WAIT, reached).await.is_err() {
            panic!("{what}: {:#?}", self.telegram.shown.lock().unwrap());
        }
    }

    async fn start(&self, (session, host, folder, pid, _): Session) {
        let post = HookPost::new(
            host.into(),
            session.into(),
            folder.into(),
            String::new(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        );
        self.hooks.send(post).await.unwrap();
    }

    /// The owner presses `data` on the menu; returns the press's answer.
    async fn press(&self, data: &str) -> Option<String> {
        let menu = self.telegram.menu().expect("the owner's menu");
        let query = {
            let mut next = self.next_query.lock().unwrap();
            *next += 1;
            format!("q{next}")
        };
        let mut update = press(&query, menu.id, data);
        {
            let mut next = self.next_update.lock().unwrap();
            *next += 1;
            update["update_id"] = json!(*next);
        }
        self.batches.send(vec![update]).unwrap();
        self.until(data, |telegram| telegram.answer(&query).is_some())
            .await;
        self.telegram.answer(&query).flatten()
    }

    /// Waits until the menu is `ready`; returns it.
    async fn menu_until(&self, what: &str, ready: impl Fn(&Shown) -> bool) -> Shown {
        self.until(what, |telegram| {
            telegram.menu().is_some_and(|menu| ready(&menu))
        })
        .await;
        self.telegram.menu().unwrap()
    }
}

/// The registry slots of the sessions, in start order.
const PRESSED_SLOT: usize = 0;
const NEIGHBOUR_SLOT: usize = 2;
const OLD_SLOT: usize = 3;

/// The row number of registry slot `slot` in the sessions tab (from its ↗).
fn row_of(menu: &Shown, slot: usize) -> usize {
    let data = format!("menu:o:0:{slot}");
    let (label, _) = menu
        .buttons
        .iter()
        .find(|(_, button)| *button == data)
        .unwrap_or_else(|| panic!("{:?}", menu.buttons));
    label.trim_start_matches("↗ ").parse().unwrap()
}

/// The lines of the row of slot `slot`, its note included.
fn row(menu: &Shown, slot: usize) -> String {
    let n = row_of(menu, slot);
    let at = menu
        .text
        .find(&format!("\n{n}. "))
        .unwrap_or_else(|| panic!("{}", menu.text));
    let rest = &menu.text[at + 1..];
    let end = rest.find(&format!("\n{}. ", n + 1)).unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// The row of slot `slot` has the button `icon n` (`n` its row number).
fn has_button(menu: &Shown, slot: usize, icon: &str) -> bool {
    let label = format!("{icon} {}", row_of(menu, slot));
    menu.buttons.iter().any(|(text, _)| *text == label)
}

// ---------------------------------------------------------------- agent

struct Agent {
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Agent {
    async fn connect(
        hub: &Hub,
        (session, host, folder, pid, can_switch): Session,
        sandbox: SandboxState,
    ) -> Self {
        let stream = TcpStream::connect(hub.agent_addr).await.unwrap();
        let (read, mut write) = stream.into_split();
        let hello = AgentMsg::Hello {
            secret: Secret::parse(SECRET).unwrap(),
        };
        wire::write_msg(&mut write, &hello).await.unwrap();
        let register = AgentMsg::Register(Register {
            session_id: session.into(),
            host: host.into(),
            cwd: folder.into(),
            claude_pid: Some(pid),
            verdict_ack: false,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            console_line_chars: 0,
            client: Some(Client {
                version: "0.1.36".into(),
                build: "e2e".into(),
                self_update: true,
            }),
            files: false,
            session_reads: false,
            status_lines: false,
            private_place: true,
            enrolled: None,
            heartbeat: false,
            sandbox: can_switch.then_some(sandbox),
        });
        wire::write_msg(&mut write, &register).await.unwrap();
        let mut agent = Self {
            reader: BufReader::new(read),
            write,
        };
        assert!(matches!(
            agent.next_within(WAIT).await,
            Some(HubMsg::Registered { .. })
        ));
        agent
    }

    async fn next_within(&mut self, wait: Duration) -> Option<HubMsg> {
        let mut line = Vec::new();
        match tokio::time::timeout(wait, wire::read_line(&mut self.reader, &mut line)).await {
            Ok(Ok(())) => Some(wire::decode(&line).unwrap()),
            _ => None,
        }
    }

    /// The first message `pick` takes within [`WAIT`]; others are skipped.
    async fn expect<T>(&mut self, what: &str, pick: impl Fn(HubMsg) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_within(left).await {
                Some(msg) => {
                    if let Some(found) = pick(msg) {
                        return found;
                    }
                }
                None => panic!("{what}: nothing came"),
            }
        }
    }

    /// Nothing `pick` takes comes within [`QUIET`].
    async fn never(&mut self, what: &str, pick: impl Fn(&HubMsg) -> bool) {
        let deadline = tokio::time::Instant::now() + QUIET;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_within(left).await {
                Some(msg) => assert!(!pick(&msg), "{what}: {msg:?}"),
                None => return,
            }
        }
    }

    async fn send(&mut self, msg: AgentMsg) {
        wire::write_msg(&mut self.write, &msg).await.unwrap();
    }

    /// The next `sandbox_set`: (request id, on).
    async fn sandbox_set(&mut self) -> (u64, bool) {
        self.expect("sandbox_set", |msg| match msg {
            HubMsg::SandboxSet { request_id, on } => Some((request_id, on)),
            _ => None,
        })
        .await
    }

    /// The next `update`: (update id, release, sandbox folder).
    async fn update(&mut self) -> (u64, Option<String>, Option<String>) {
        self.expect("update", |msg| match msg {
            HubMsg::Update {
                update_id,
                release,
                sandbox_folder,
            } => Some((update_id, release, sandbox_folder)),
            _ => None,
        })
        .await
    }

    async fn update_answer(&mut self, update_id: u64, outcome: UpdateOutcome) {
        self.send(AgentMsg::UpdateAnswer { update_id, outcome })
            .await;
    }

    async fn sandbox_answer(
        &mut self,
        request_id: u64,
        outcome: SandboxOutcome,
        reason: Option<&str>,
        state: SandboxState,
    ) {
        self.send(AgentMsg::SandboxAnswer {
            request_id,
            outcome,
            reason: reason.map(str::to_owned),
            state: Some(state),
            folder: Some(FOLDER.into()),
            restart: true,
        })
        .await;
    }
}

fn is_update(msg: &HubMsg) -> bool {
    matches!(msg, HubMsg::Update { .. })
}

fn is_sandbox_set(msg: &HubMsg) -> bool {
    matches!(msg, HubMsg::SandboxSet { .. })
}

/// The agents of the test.
struct Agents {
    pressed: Agent,
    inside: Agent,
    neighbour: Agent,
    old: Agent,
    elsewhere: Agent,
}

/// The switch of the pressed session's folder to `on`, after the press:
/// the agent marks it, the menu shows `note`, every live session of `h1`
/// that can take it gets the quiet restart (the inside one and the
/// neighbour answer as their agents decide: their mode is as marked; the
/// old agent and `h2` get none), the pressed session restarts, and its
/// next agent comes in the new mode.
async fn switch(hub: &Hub, agents: &mut Agents, on: bool, note: &str) {
    let (request_id, asked_on) = agents.pressed.sandbox_set().await;
    assert_eq!(asked_on, on);
    agents
        .pressed
        .sandbox_answer(request_id, SandboxOutcome::Done, None, mode(!on, on))
        .await;
    let menu = hub
        .menu_until("the answer under the row", |menu| {
            row(menu, PRESSED_SLOT).contains(note)
        })
        .await;
    let pending = if on {
        ", 🔒 после перезапуска"
    } else {
        ", сэндбокс снимется после перезапуска"
    };
    assert!(row(&menu, PRESSED_SLOT).contains(pending), "{}", menu.text);
    let (update_id, release, folder) = agents.pressed.update().await;
    assert_eq!((release, folder.as_deref()), (None, Some(FOLDER)));
    for other in [&mut agents.inside, &mut agents.neighbour] {
        let (id, release, folder) = other.update().await;
        assert_eq!((release, folder.as_deref()), (None, Some(FOLDER)));
        other.update_answer(id, UpdateOutcome::UpToDate).await;
    }
    for agent in [&mut agents.old, &mut agents.elsewhere] {
        agent.never("no quiet restart", is_update).await;
    }
    agents
        .pressed
        .update_answer(update_id, UpdateOutcome::Restarting)
        .await;
    agents
        .pressed
        .expect("released", |msg| match msg {
            HubMsg::Released { update_id: id, .. } if id == update_id => Some(()),
            _ => None,
        })
        .await;
    // claude restarted in the new mode: its next agent, asked once more.
    agents.pressed = Agent::connect(hub, PRESSED, mode(on, on)).await;
    let (id, _, folder) = agents.pressed.update().await;
    assert_eq!(folder.as_deref(), Some(FOLDER));
    agents
        .pressed
        .update_answer(id, UpdateOutcome::UpToDate)
        .await;
}

// ---------------------------------------------------------------- the test

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_switches_a_folder_into_the_sandbox_and_back_from_the_menu() {
    let hub = start_hub().await;
    for session in [PRESSED, INSIDE, NEIGHBOUR, OLD, ELSEWHERE] {
        hub.start(session).await;
    }
    let off = mode(false, false);
    let mut agents = Agents {
        pressed: Agent::connect(&hub, PRESSED, off).await,
        inside: Agent::connect(&hub, INSIDE, off).await,
        neighbour: Agent::connect(&hub, NEIGHBOUR, off).await,
        old: Agent::connect(&hub, OLD, off).await,
        elsewhere: Agent::connect(&hub, ELSEWHERE, off).await,
    };
    hub.until("five topics and the owner's menu", |telegram| {
        let topics = telegram
            .ops
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, Op::CreateTopic { chat, .. } if *chat == owner_chat()))
            .count();
        topics == 5 && telegram.menu().is_some()
    })
    .await;
    hub.press("menu:s:0").await;
    let menu = hub
        .menu_until("five rows", |menu| menu.text.contains("\n5. "))
        .await;
    assert!(has_button(&menu, PRESSED_SLOT, "🔒"), "{:?}", menu.buttons);
    assert!(
        !row(&menu, PRESSED_SLOT).contains("сэндбокс"),
        "{}",
        menu.text
    );

    // On: the press is answered before the agent did anything.
    assert_eq!(
        hub.press(&format!("menu:sb:0:{PRESSED_SLOT}"))
            .await
            .as_deref(),
        Some("Проверяю устройство…")
    );
    switch(
        &hub,
        &mut agents,
        true,
        "включится после перезапуска сессии",
    )
    .await;
    hub.press("menu:s:0").await;
    let menu = hub
        .menu_until("sandboxed", |menu| {
            row(menu, PRESSED_SLOT).contains(", 🔒 сэндбокс")
        })
        .await;
    assert!(!menu.text.contains("перезапуск"), "{}", menu.text);
    assert!(has_button(&menu, PRESSED_SLOT, "🔓"), "{:?}", menu.buttons);

    // Off: asked once more, then switched.
    assert_eq!(
        hub.press(&format!("menu:sf:0:{PRESSED_SLOT}"))
            .await
            .as_deref(),
        Some("Нажмите ещё раз, чтобы выключить сэндбокс папки")
    );
    let menu = hub
        .menu_until("«точно?»", |menu| {
            let label = format!("🔓 {} точно?", row_of(menu, PRESSED_SLOT));
            menu.buttons.iter().any(|(text, _)| *text == label)
        })
        .await;
    assert!(!has_button(&menu, PRESSED_SLOT, "🔓"), "{:?}", menu.buttons);
    agents
        .pressed
        .never("not before «точно?»", is_sandbox_set)
        .await;
    assert_eq!(
        hub.press(&format!("menu:sfc:0:{PRESSED_SLOT}"))
            .await
            .as_deref(),
        Some("Выключаю…")
    );
    switch(
        &hub,
        &mut agents,
        false,
        "снимется после перезапуска сессии",
    )
    .await;
    hub.press("menu:s:0").await;
    let menu = hub
        .menu_until("off again", |menu| has_button(menu, PRESSED_SLOT, "🔒"))
        .await;
    assert!(
        !row(&menu, PRESSED_SLOT).contains("сэндбокс"),
        "{}",
        menu.text
    );
    assert!(!menu.text.contains("перезапуск"), "{}", menu.text);

    // The old agent cannot switch: told so, nothing sent.
    assert_eq!(
        hub.press(&format!("menu:sb:0:{OLD_SLOT}")).await.as_deref(),
        Some("Этот клиент старше обновлений из Telegram: перезапустите сессию вручную")
    );
    agents.old.never("an old agent", is_sandbox_set).await;

    // A refusal: its reason under the row, no restart anywhere.
    assert_eq!(
        hub.press(&format!("menu:sb:0:{NEIGHBOUR_SLOT}"))
            .await
            .as_deref(),
        Some("Проверяю устройство…")
    );
    let (request_id, true) = agents.neighbour.sandbox_set().await else {
        panic!("on");
    };
    agents
        .neighbour
        .sandbox_answer(
            request_id,
            SandboxOutcome::Refused,
            Some("не найдена программа bwrap"),
            off,
        )
        .await;
    hub.menu_until("the reason", |menu| {
        row(menu, NEIGHBOUR_SLOT).contains("\n   не переключено: не найдена программа bwrap")
    })
    .await;
    for agent in [
        &mut agents.pressed,
        &mut agents.inside,
        &mut agents.neighbour,
        &mut agents.elsewhere,
    ] {
        agent.never("no restart after a refusal", is_update).await;
    }

    // No topic ever showed a word of it.
    let topics = hub.telegram.topic_ops();
    assert!(!topics.is_empty(), "the topics were made");
    for op in &topics {
        for word in ["сэндбокс", "Сэндбокс", "🔒", "🔓", "sandbox"] {
            assert!(!op.contains(word), "{word} in a topic: {op}");
        }
    }
}

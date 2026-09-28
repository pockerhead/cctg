//! The owner's private view end to end (TASK-063): an agent over a real TCP
//! link to the real `serve_agents` (a hand-written client, like
//! `status_e2e.rs`), hook events straight into the real `Slots` actor, the
//! real per-chat `Outbox`, and a fake Bot API that keeps every chat on its
//! own: topics and messages are numbered per chat, as Telegram does, so a
//! message of the group and its twin in the private chat have different
//! ids. Private chats are on (`Options::owners`) with one owner.

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::chat::{Chat, Place, PrivateChat};
use cctg::hub::ingress::{bind, serve_agents};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{
    Control, ECHO_MARK, FALLBACK_END_NOTICE, Options, Owners, PRIVATE_CLOSED_NOTICE,
    PRIVATE_GENERAL_NOTICE, PRIVATE_START_TEXT, Slots,
};
use cctg::hub::updates::{CallbackInput, Inbound};
use cctg::hub::{permissions, updates};
use cctg::wire::{
    self, AgentMsg, Behavior, HookEvent, HookPost, HubMsg, PermissionRequest, Register, Secret,
};
use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod common;

const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const CWD: &str = "C:/qa/private";
const SESSION: &str = "0a16e2e0-0000-4000-8000-000000000631";
/// Distinctive: it must never show in a text or a meta.
const OWNER: i64 = 7_319_402_518;
const WAIT: Duration = Duration::from_secs(30);
/// The name Telegram gives the owner.
const NAME: &str = "Анна";

/// The echo of `text` in the slot's other view (TASK-063).
fn echo(text: &str) -> String {
    format!("{ECHO_MARK} {NAME}: {text}")
}

fn owner() -> Chat {
    Chat::Private(PrivateChat::of_user(OWNER))
}

// ---------------------------------------------------------------- fake Telegram

/// A message a topic shows.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    id: i64,
    text: String,
    /// The texts of its buttons.
    buttons: Vec<String>,
    loud: bool,
    /// Sent with buttons and not as a prompt: a status message.
    status: bool,
}

#[derive(Default)]
struct ChatModel {
    next_topic: i64,
    next_message: i64,
    topics: BTreeMap<i64, Vec<Shown>>,
    /// Messages outside a topic (General).
    general: Vec<String>,
}

#[derive(Default)]
struct Fake {
    chats: Mutex<HashMap<Chat, ChatModel>>,
    ops: Mutex<Vec<Op>>,
    /// Every call into a private chat is refused with 403 (no Start yet).
    forbid_private: AtomicBool,
}

fn buttons(markup: Option<&Value>) -> Vec<String> {
    markup
        .and_then(|markup| markup["inline_keyboard"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|button| button["text"].as_str().map(str::to_owned))
        .collect()
}

impl ChatModel {
    fn message(&mut self, id: i64) -> Option<&mut Shown> {
        self.topics
            .values_mut()
            .flat_map(|topic| topic.iter_mut())
            .find(|shown| shown.id == id)
    }

    fn post(&mut self, base: i64, thread: Option<i64>, shown: Shown) -> Result<i64, ApiError> {
        self.next_message = self.next_message.max(base) + 1;
        let id = self.next_message;
        let Some(thread) = thread else {
            self.general.push(shown.text);
            return Ok(id);
        };
        let Some(topic) = self.topics.get_mut(&thread) else {
            return Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message thread not found".into(),
            });
        };
        topic.push(Shown { id, ..shown });
        Ok(id)
    }
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        let Some(chat) = op.chat() else {
            return Ok(Outcome::Done);
        };
        if chat.is_private() && self.forbid_private.load(Ordering::SeqCst) {
            return Err(ApiError::Telegram {
                code: 403,
                description: "Forbidden: bot can't initiate conversation with a user".into(),
            });
        }
        // Ids of the group and of the private chat never meet.
        let base = if chat.is_private() { 5000 } else { 1000 };
        let mut chats = self.chats.lock().unwrap();
        let model = chats.entry(chat).or_default();
        let sent = |id| {
            Ok(Outcome::Sent(Message {
                message_id: id,
                ..Message::default()
            }))
        };
        let not_found = || {
            Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message to edit not found".into(),
            })
        };
        match op {
            Op::CreateTopic { name, .. } => {
                model.next_topic = model.next_topic.max(base / 10) + 1;
                model.topics.insert(model.next_topic, Vec::new());
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: model.next_topic,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send {
                thread_id,
                text,
                reply_markup,
                permission,
                notify,
                ..
            } => {
                let shown = Shown {
                    id: 0,
                    text: text.clone(),
                    buttons: buttons(reply_markup.as_ref()),
                    loud: *notify,
                    status: reply_markup.is_some() && !permission,
                };
                model.post(base, *thread_id, shown).and_then(sent)
            }
            Op::Stream {
                thread_id,
                text,
                notify,
                into: None,
                ..
            } => {
                let shown = Shown {
                    id: 0,
                    text: text.clone(),
                    buttons: Vec::new(),
                    loud: *notify,
                    status: false,
                };
                model.post(base, Some(*thread_id), shown).and_then(sent)
            }
            Op::Stream {
                text,
                into: Some(id),
                ..
            } => match model.message(*id) {
                Some(shown) => {
                    shown.text.clone_from(text);
                    shown.buttons.clear();
                    shown.status = false;
                    sent(*id)
                }
                None => not_found(),
            },
            Op::Edit {
                message_id,
                text,
                reply_markup,
                ..
            } => match model.message(*message_id) {
                Some(shown) => {
                    shown.text.clone_from(text);
                    if reply_markup.is_some() {
                        shown.buttons = buttons(reply_markup.as_ref());
                    }
                    Ok(Outcome::Done)
                }
                None => not_found(),
            },
            Op::Delete { message_id, .. } => {
                for topic in model.topics.values_mut() {
                    topic.retain(|shown| shown.id != *message_id);
                }
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }

    /// The one topic of `chat`, once made.
    fn topic(&self, chat: Chat) -> Option<i64> {
        let chats = self.chats.lock().unwrap();
        chats.get(&chat)?.topics.keys().next_back().copied()
    }

    /// What the topic of `chat` shows.
    fn shown(&self, chat: Chat) -> Vec<Shown> {
        let chats = self.chats.lock().unwrap();
        chats
            .get(&chat)
            .and_then(|model| model.topics.values().next_back().cloned())
            .unwrap_or_default()
    }

    /// The texts of the topic of `chat`, `STATUS` for the status message.
    fn layout(&self, chat: Chat) -> Vec<String> {
        self.shown(chat)
            .into_iter()
            .map(|m| if m.status { "STATUS".into() } else { m.text })
            .collect()
    }

    fn general(&self, chat: Chat) -> Vec<String> {
        let chats = self.chats.lock().unwrap();
        chats
            .get(&chat)
            .map(|model| model.general.clone())
            .unwrap_or_default()
    }

    /// A message of the user in the topic of `chat`: its id.
    fn user(&self, chat: Chat, text: &str) -> i64 {
        let base = if chat.is_private() { 5000 } else { 1000 };
        let mut chats = self.chats.lock().unwrap();
        let model = chats.entry(chat).or_default();
        let thread = model.topics.keys().next_back().copied();
        let shown = Shown {
            id: 0,
            text: text.into(),
            buttons: Vec::new(),
            loud: false,
            status: false,
        };
        model.post(base, thread, shown).expect("the topic is there")
    }

    /// The user deletes the topic of `chat` (a private chat lets them).
    fn delete_topic(&self, chat: Chat) {
        let mut chats = self.chats.lock().unwrap();
        let model = chats.entry(chat).or_default();
        if let Some(&thread) = model.topics.keys().next_back() {
            model.topics.remove(&thread);
        }
    }
}

// ---------------------------------------------------------------- hub

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Hub {
    fake: Arc<Fake>,
    agent_addr: SocketAddr,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
    tasks: Vec<JoinHandle<()>>,
    _state: TempRoot,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Private chats on, and new slots shared to the group too: a slot as one
/// made in the group before TASK-063, mirrored there.
const SHARED: Mode = Mode::Private { share_new: true };
/// Private chats on as the hub runs: a new slot shows in the private chat
/// alone (decision 2026-09-28).
const PRIVATE: Mode = Mode::Private { share_new: false };

#[derive(Clone, Copy)]
enum Mode {
    /// The bot has no topics in private chats.
    Group,
    Private {
        share_new: bool,
    },
}

/// A hub on a fresh state.
async fn start_hub(name: &str, mode: Mode, fake: Fake) -> Hub {
    let state = fresh_state(name);
    start_hub_on(&state, mode, Arc::new(fake), true).await
}

fn fresh_state(name: &str) -> PathBuf {
    let state =
        std::env::temp_dir().join(format!("cctg-private-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    state
}

/// A hub on `state` as a hub before left it (`registry.json`), talking to
/// `fake`; `own`: it removes `state` when dropped.
async fn start_hub_on(state: &std::path::Path, mode: Mode, fake: Arc<Fake>, own: bool) -> Hub {
    let fast = Limits::from(BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    });
    let outbox = Outbox::per_chat(fake.clone(), fast, fast);
    let store = RegistryStore::open(state).unwrap();
    let registry = store.load().unwrap();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(Duration::from_millis(50)),
        owners: match mode {
            Mode::Group => None,
            Mode::Private { share_new } => Some(Owners {
                first: PrivateChat::of_user(OWNER),
                devices: None,
                share_new,
            }),
        },
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
    let tasks = vec![
        tokio::spawn(serve_agents(
            listener,
            Secret::parse(SECRET).unwrap(),
            agents,
        )),
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)),
    ];
    // A hub that hands its state on removes nothing.
    let keep = if own {
        state.to_path_buf()
    } else {
        state.join("never-there")
    };
    Hub {
        fake,
        agent_addr,
        hooks,
        control,
        tasks,
        _state: TempRoot(keep),
    }
}

impl Hub {
    async fn hook(&self, event: HookEvent) {
        let post = HookPost::new(
            HOST.into(),
            SESSION.into(),
            CWD.into(),
            String::new(),
            event,
        );
        self.hooks.send(post).await.unwrap();
    }

    async fn start(&self) {
        self.hook(HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        })
        .await;
    }

    /// Waits until `ready` holds.
    async fn until(&self, what: &str, ready: impl Fn(&Fake) -> bool) {
        let reached = async {
            while !ready(&self.fake) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(WAIT, reached).await.is_err() {
            panic!(
                "{what}: private {:#?} group {:#?}",
                self.fake.shown(owner()),
                self.fake.shown(Chat::Group)
            );
        }
    }

    /// Waits until both views show `want`.
    async fn both(&self, what: &str, want: &[&str]) {
        self.until(what, |fake| {
            fake.layout(owner()) == want && fake.layout(Chat::Group) == want
        })
        .await;
    }

    /// A user's message in the topic of `chat`, handed over as the poll does.
    fn say(&self, chat: Chat, text: &str) -> i64 {
        let message_id = self.fake.user(chat, text);
        self.control
            .send(Control::Message(Inbound {
                chat,
                sender: PrivateChat::of_user(OWNER),
                message_id,
                thread_id: self.fake.topic(chat),
                text: Some(text.into()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
                from_name: None,
                author: Some(NAME.into()),
            }))
            .unwrap();
        message_id
    }

    /// A user's message in the General of `chat`.
    fn say_general(&self, chat: Chat, text: &str) {
        self.control
            .send(Control::Message(Inbound {
                chat,
                sender: PrivateChat::of_user(OWNER),
                message_id: 1,
                thread_id: None,
                text: Some(text.into()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
                from_name: None,
                author: None,
            }))
            .unwrap();
    }

    /// A press of the button `data` of message `message_id` in `chat`.
    fn press(&self, chat: Chat, message_id: i64, data: &str) {
        self.control
            .send(Control::Callback(CallbackInput {
                chat: Some(chat),
                query_id: format!("q-{data}-{message_id}"),
                data: Some(data.into()),
                message_id: Some(message_id),
                thread_id: self.fake.topic(chat),
                from_name: None,
            }))
            .unwrap();
    }
}

// ---------------------------------------------------------------- agent

struct Agent {
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Agent {
    /// `private_place`: it tells a private chat's message from the group's.
    async fn connect(hub: &Hub, private_place: bool) -> Self {
        let stream = TcpStream::connect(hub.agent_addr).await.unwrap();
        let (read, mut write) = stream.into_split();
        let hello = AgentMsg::Hello {
            secret: Secret::parse(SECRET).unwrap(),
        };
        wire::write_msg(&mut write, &hello).await.unwrap();
        let register = AgentMsg::Register(Register {
            session_id: SESSION.into(),
            host: HOST.into(),
            cwd: CWD.into(),
            claude_pid: Some(4242),
            verdict_ack: true,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            client: None,
            files: false,
            session_reads: false,
            status_lines: false,
            private_place,
            enrolled: None,
            heartbeat: false,
        });
        wire::write_msg(&mut write, &register).await.unwrap();
        let mut agent = Self {
            reader: BufReader::new(read),
            write,
        };
        assert!(matches!(
            agent.next().await,
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

    async fn next(&mut self) -> Option<HubMsg> {
        self.next_within(WAIT).await
    }

    /// The next inbound message: its content and meta.
    async fn inbound(&mut self) -> (String, BTreeMap<String, String>) {
        loop {
            match self.next().await {
                Some(HubMsg::Inbound { content, meta }) => return (content, meta),
                Some(_) => {}
                None => panic!("no inbound message"),
            }
        }
    }

    async fn send(&mut self, msg: AgentMsg) {
        wire::write_msg(&mut self.write, &msg).await.unwrap();
    }
}

fn permission(request_id: &str) -> AgentMsg {
    AgentMsg::PermissionRequest(PermissionRequest {
        request_id: request_id.into(),
        tool_name: "Bash".into(),
        description: "Run a command".into(),
        input_preview: "ls".into(),
    })
}

/// The prompt of `request_id` as `chat` shows it.
fn prompt_in(fake: &Fake, chat: Chat, request_id: &str) -> Option<Shown> {
    let _ = request_id;
    fake.shown(chat)
        .into_iter()
        .find(|shown| shown.text.starts_with("Запрос разрешения"))
}

/// The prompts the topic of `chat` shows.
fn prompts_in(fake: &Fake, chat: Chat) -> usize {
    fake.shown(chat)
        .iter()
        .filter(|shown| shown.text.starts_with("Запрос разрешения"))
        .count()
}

/// The prompt shows in `chat` with its buttons.
fn asks_in(fake: &Fake, chat: Chat, request_id: &str) -> bool {
    prompt_in(fake, chat, request_id)
        .is_some_and(|prompt| prompt.buttons == ["Разрешить", "Запретить"])
}

/// Presses Allow on the prompt of `request_id` in `chat`: the agent gets
/// the one verdict and acks it.
async fn allow_in(hub: &Hub, agent: &mut Agent, chat: Chat, request_id: &str) {
    let message = prompt_in(&hub.fake, chat, request_id).unwrap().id;
    hub.press(chat, message, &format!("allow:{request_id}"));
    let verdict_id = loop {
        match agent.next().await {
            Some(HubMsg::PermissionVerdict {
                request_id: got,
                behavior: Behavior::Allow,
                verdict_id: Some(verdict_id),
            }) if got == request_id => break verdict_id,
            Some(HubMsg::Inbound { .. }) | None => panic!("no verdict"),
            Some(_) => {}
        }
    };
    agent.send(AgentMsg::PermissionAck { verdict_id }).await;
}

/// The prompt of `request_id` in `chat` shows the decision, no buttons.
fn allowed_in(fake: &Fake, chat: Chat, request_id: &str) -> bool {
    prompt_in(fake, chat, request_id).is_some_and(|prompt| {
        prompt.text.ends_with(permissions::ALLOWED_MARK) && prompt.buttons.is_empty()
    })
}

/// Every text Telegram got, for the leak check.
fn texts(ops: &[Op]) -> String {
    format!("{ops:?}")
}

// ---------------------------------------------------------------- tests

/// A new session as the hub runs (decision 2026-09-28): a topic in the
/// owner's private chat alone; nothing goes into the group.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_new_session_shows_in_the_private_chat_alone() {
    let hub = start_hub("alone", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "привет".into(),
        })
        .await;
    hub.until("the reply in the private chat", |fake| {
        fake.layout(owner()) == ["привет", "STATUS"]
    })
    .await;
    hub.say(owner(), "из лички");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из лички", "private")
    );
    assert!(
        hub.fake
            .ops()
            .iter()
            .all(|op| op.chat().is_none_or(|chat| chat != Chat::Group)),
        "{:#?}",
        hub.fake.ops()
    );
}

/// A shared slot (one made in the group before): a topic in the owner's
/// private chat and in the group; the status, a reply and the turn's
/// answer show in both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_new_session_shows_in_the_private_chat_and_the_group() {
    let hub = start_hub("new", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;

    agent
        .send(AgentMsg::Reply {
            text: "привет".into(),
        })
        .await;
    hub.both("the reply in both views", &["привет", "STATUS"])
        .await;
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some("Готово".into()),
    })
    .await;
    hub.both("the answer in both views", &["привет", "Готово", "STATUS"])
        .await;
    for chat in [owner(), Chat::Group] {
        let answer = hub
            .fake
            .shown(chat)
            .into_iter()
            .find(|shown| shown.text == "Готово")
            .unwrap();
        assert!(answer.loud, "the answer rings in {chat:?}");
    }
    // Ids are the chat's own: the twins have other ids than the primaries.
    let private_ids: Vec<i64> = hub.fake.shown(owner()).iter().map(|m| m.id).collect();
    let group_ids: Vec<i64> = hub.fake.shown(Chat::Group).iter().map(|m| m.id).collect();
    assert!(private_ids.iter().all(|id| *id > 5000), "{private_ids:?}");
    assert!(group_ids.iter().all(|id| *id < 5000), "{group_ids:?}");
    // The private chat's id is only the request's address, never a text.
    let ops = hub.fake.ops();
    for op in &ops {
        let text = match op {
            Op::Send { text, .. } | Op::Edit { text, .. } | Op::Stream { text, .. } => text,
            _ => continue,
        };
        assert!(!text.contains(&OWNER.to_string()), "{text}");
    }
    assert!(!texts(&ops).contains(&OWNER.to_string()));
}

/// A message written in either view reaches the session once, with its
/// place, and shows in the other view as an echo signed with its author's
/// name; the status message moves below it in both views.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_message_from_either_view_reaches_the_session() {
    let hub = start_hub("inbound", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;

    let private_id = hub.say(owner(), "из лички");
    let (content, meta) = agent.inbound().await;
    assert_eq!(content, "из лички");
    assert_eq!(meta["place"], "private");
    assert_eq!(meta["message_id"], private_id.to_string());
    assert_eq!(
        meta["thread_id"],
        hub.fake.topic(owner()).unwrap().to_string()
    );
    assert!(!format!("{meta:?}").contains(&OWNER.to_string()));
    // The echo goes once the burst reached the session; someone who writes
    // in the group before that sees it after their own message, which is
    // not what this test is about.
    hub.until("the echo in the group", |fake| {
        fake.layout(Chat::Group).contains(&echo("из лички"))
    })
    .await;

    let group_id = hub.say(Chat::Group, "из группы");
    let (content, meta) = agent.inbound().await;
    assert_eq!(content, "из группы");
    assert_eq!(meta["place"], "group");
    assert_eq!(meta["message_id"], group_id.to_string());
    // Each view shows what was written in the other, signed, and the
    // status goes below it all in both.
    hub.until("the echoes, and the status below them", |fake| {
        fake.layout(owner()) == ["из лички".to_owned(), echo("из группы"), "STATUS".into()]
            && fake.layout(Chat::Group)
                == [echo("из лички"), "из группы".to_owned(), "STATUS".into()]
    })
    .await;
    // The session got each message once: no third inbound.
    assert!(
        !matches!(
            agent.next_within(Duration::from_millis(300)).await,
            Some(HubMsg::Inbound { .. })
        ),
        "an echo never reaches the session"
    );
}

/// A prompt shows in both views; a press in either decides it, later
/// presses decide nothing, and both messages are edited to the decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_press_in_either_view_decides_the_prompt_and_both_are_edited() {
    let hub = start_hub("press", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;

    for (request_id, pressed, other) in [
        ("abcde", Chat::Group, owner()),
        ("fghij", owner(), Chat::Group),
    ] {
        agent.send(permission(request_id)).await;
        hub.until("the prompt in both views", |fake| {
            [owner(), Chat::Group].into_iter().all(|chat| {
                prompt_in(fake, chat, request_id)
                    .is_some_and(|prompt| prompt.buttons == ["Разрешить", "Запретить"])
            })
        })
        .await;
        let message = prompt_in(&hub.fake, pressed, request_id).unwrap().id;
        hub.press(pressed, message, &format!("allow:{request_id}"));
        let verdict_id = match agent.next().await {
            Some(HubMsg::PermissionVerdict {
                request_id: got,
                behavior: Behavior::Allow,
                verdict_id: Some(verdict_id),
            }) if got == request_id => verdict_id,
            other => panic!("no verdict: {other:?}"),
        };
        agent.send(AgentMsg::PermissionAck { verdict_id }).await;
        hub.until("both prompts show the decision without buttons", |fake| {
            [owner(), Chat::Group].into_iter().all(|chat| {
                prompt_in(fake, chat, request_id).is_some_and(|prompt| {
                    prompt.text.ends_with(permissions::ALLOWED_MARK) && prompt.buttons.is_empty()
                })
            })
        })
        .await;
        // A press in the other view decides nothing.
        let twin = prompt_in(&hub.fake, other, request_id).unwrap().id;
        hub.press(other, twin, &format!("deny:{request_id}"));
        let answered = |fake: &Fake| {
            fake.ops().iter().any(|op| {
                matches!(op, Op::AnswerCallback { query_id, text: Some(text) }
                    if query_id == &format!("q-deny:{request_id}-{twin}")
                        && text == permissions::ANSWER_DECIDED)
            })
        };
        hub.until("the later press is told it is decided", answered)
            .await;
        assert!(
            agent
                .next_within(Duration::from_millis(300))
                .await
                .is_none(),
            "no second verdict"
        );
        // The next prompt is the only one each view shows.
        for chat in [owner(), Chat::Group] {
            let message = prompt_in(&hub.fake, chat, request_id).unwrap().id;
            hub.fake
                .chats
                .lock()
                .unwrap()
                .get_mut(&chat)
                .unwrap()
                .topics
                .values_mut()
                .for_each(|topic| topic.retain(|shown| shown.id != message));
        }
    }
}

/// An agent built before TASK-063 (it does not announce `private_place`;
/// v0.1.13-15 read the place right, older ones do not) still gets the
/// private chat's messages, with their place: they are what the owner
/// writes now. Only their ✍ is not tracked (`Slots::receipts_of`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_an_old_agent_gets_the_private_chats_messages_too() {
    let hub = start_hub("old-agent", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, false).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;

    hub.say(owner(), "из лички");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из лички", "private")
    );
    hub.say(Chat::Group, "из группы");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из группы", "group")
    );
}

/// A private chat whose user never pressed Start (403): the group is told
/// once and the session goes on in the group alone; once the user presses
/// Start, the private view comes and takes the session, and the slot
/// leaves the group (it was there only for the 403): its group topic is
/// told, loses its status message and gets nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_private_chat_without_start_leaves_the_session_in_the_group() {
    let fake = Fake::default();
    fake.forbid_private.store(true, Ordering::SeqCst);
    let hub = start_hub("no-start", PRIVATE, fake).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the group has the status, General the notice", |fake| {
        fake.layout(Chat::Group) == ["STATUS"]
            && fake.general(Chat::Group) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "только в группе".into(),
        })
        .await;
    hub.until("the reply in the group", |fake| {
        fake.layout(Chat::Group) == ["только в группе", "STATUS"]
    })
    .await;

    // Start: Telegram sends `/start` into the private chat's General.
    hub.fake.forbid_private.store(false, Ordering::SeqCst);
    hub.say_general(owner(), "/start");
    hub.until("the private chat is answered and gets the topic", |fake| {
        fake.general(owner()) == [PRIVATE_START_TEXT] && fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.until("the group topic is told and has no status", |fake| {
        fake.layout(Chat::Group) == ["только в группе", FALLBACK_END_NOTICE]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "в личке".into(),
        })
        .await;
    hub.until("the next reply in the private chat alone", |fake| {
        fake.layout(owner()) == ["в личке", "STATUS"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hub.fake.layout(Chat::Group),
        ["только в группе", FALLBACK_END_NOTICE],
        "nothing more in the group"
    );
    assert_eq!(
        hub.fake.general(Chat::Group),
        [PRIVATE_CLOSED_NOTICE],
        "told once"
    );
}

/// A private-only slot whose owner blocks the bot for a while (TASK-063):
/// the reply that hit the 403 goes to the group, which takes the session
/// meanwhile; the private status message stays where it was. Once the
/// owner writes again, the slot is private-only again, and its private
/// status message is the one live status there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_blocked_bot_moves_a_private_slot_to_the_group_and_back() {
    let hub = start_hub("blocked", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.fake.forbid_private.store(true, Ordering::SeqCst);
    agent
        .send(AgentMsg::Reply {
            text: "в заблокированную".into(),
        })
        .await;
    hub.until("the reply lost to the 403 is in the group", |fake| {
        fake.layout(Chat::Group) == ["в заблокированную", "STATUS"]
            && fake.general(Chat::Group) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    assert_eq!(hub.fake.layout(owner()), ["STATUS"], "the old status stays");

    // The owner writes in the private topic: it opens again.
    hub.fake.forbid_private.store(false, Ordering::SeqCst);
    hub.say(owner(), "снова тут");
    let (content, _) = agent.inbound().await;
    assert_eq!(content, "снова тут");
    hub.until("the slot leaves the group", |fake| {
        fake.layout(Chat::Group) == ["в заблокированную", FALLBACK_END_NOTICE]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "в личке".into(),
        })
        .await;
    hub.until("one live status, below the new messages", |fake| {
        fake.layout(owner()) == ["снова тут", "в личке", "STATUS"]
    })
    .await;
}

/// A message in the private chat's General other than `/start` reaches no
/// session and is answered once a minute with where to write; `/start` in
/// a chat that is open already gets the start text again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_the_private_general_answers_where_to_write() {
    let hub = start_hub("general", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.say_general(owner(), "привет");
    hub.say_general(owner(), "ещё");
    hub.say_general(owner(), "/start");
    hub.until("both answers in General", |fake| {
        fake.general(owner()) == [PRIVATE_GENERAL_NOTICE, PRIVATE_START_TEXT]
    })
    .await;
    assert!(
        !matches!(
            agent.next_within(Duration::from_millis(300)).await,
            Some(HubMsg::Inbound { .. })
        ),
        "General reaches no session"
    );
}

/// The user deletes the private topic of a shared slot and the next
/// message of the session is a permission prompt (code review TASK-063):
/// the prompt is not lost. Its twin in the group stays, it goes again into
/// the private topic made again, and a press there decides it in both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_prompt_into_a_deleted_private_topic_of_a_shared_slot_goes_again() {
    let hub = start_hub("prompt-deleted-shared", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;
    let first = hub.fake.topic(owner()).unwrap();
    hub.fake.delete_topic(owner());
    agent.send(permission("abcde")).await;
    hub.until(
        "the prompt in the new private topic and in the group",
        |fake| {
            fake.topic(owner()).is_some_and(|topic| topic != first)
                && asks_in(fake, owner(), "abcde")
                && asks_in(fake, Chat::Group, "abcde")
        },
    )
    .await;
    assert_eq!(prompts_in(&hub.fake, Chat::Group), 1, "the twin is kept");
    allow_in(&hub, &mut agent, owner(), "abcde").await;
    hub.until("both show the decision", |fake| {
        allowed_in(fake, owner(), "abcde") && allowed_in(fake, Chat::Group, "abcde")
    })
    .await;
    assert_eq!(prompts_in(&hub.fake, Chat::Group), 1);
    assert_eq!(prompts_in(&hub.fake, owner()), 1);
}

/// The same for a slot in the private chat alone: the prompt goes into the
/// topic made again, and nothing goes to the group.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_prompt_into_a_deleted_private_topic_goes_again() {
    let hub = start_hub("prompt-deleted-private", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    let first = hub.fake.topic(owner()).unwrap();
    hub.fake.delete_topic(owner());
    agent.send(permission("abcde")).await;
    hub.until("the prompt in the new private topic", |fake| {
        fake.topic(owner()).is_some_and(|topic| topic != first) && asks_in(fake, owner(), "abcde")
    })
    .await;
    allow_in(&hub, &mut agent, owner(), "abcde").await;
    hub.until("it shows the decision", |fake| {
        allowed_in(fake, owner(), "abcde")
    })
    .await;
    assert!(
        hub.fake
            .ops()
            .iter()
            .all(|op| op.chat().is_none_or(|chat| chat != Chat::Group)),
        "{:#?}",
        hub.fake.ops()
    );
}

/// A shared slot whose owner blocked the bot (403) gets a permission
/// prompt: the group shows it once, with its buttons (its twin is the
/// prompt now), and a press there decides it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_prompt_that_meets_a_403_is_the_groups() {
    let hub = start_hub("prompt-403", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;
    hub.fake.forbid_private.store(true, Ordering::SeqCst);
    agent.send(permission("abcde")).await;
    hub.until("the prompt in the group, the notice in General", |fake| {
        asks_in(fake, Chat::Group, "abcde") && fake.general(Chat::Group) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(prompts_in(&hub.fake, Chat::Group), 1, "not sent twice");
    allow_in(&hub, &mut agent, Chat::Group, "abcde").await;
    hub.until("the group shows the decision", |fake| {
        allowed_in(fake, Chat::Group, "abcde")
    })
    .await;
    assert_eq!(prompts_in(&hub.fake, Chat::Group), 1);
}

/// The first minutes after the hub update (code review TASK-063): a slot a
/// hub before made in the group (topic, live session, status message) gets
/// a private topic; each view ends with one status message, a reply shows
/// in both, and a message from either view reaches the session once, with
/// its echo in the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_after_the_hub_update_a_group_slot_gets_its_private_view() {
    let state = fresh_state("update");
    let fake = Arc::new(Fake::default());
    let old = start_hub_on(&state, Mode::Group, fake.clone(), false).await;
    old.start().await;
    let mut agent = Agent::connect(&old, true).await;
    old.until("status in the group", |f| {
        f.layout(Chat::Group) == ["STATUS"]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "до".into()
        })
        .await;
    old.until("reply in the group", |f| {
        f.layout(Chat::Group) == ["до", "STATUS"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(agent);
    drop(old);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let new = start_hub_on(&state, PRIVATE, fake.clone(), true).await;
    let mut agent = Agent::connect(&new, true).await;
    new.until("a private topic with the status", |f| {
        f.layout(owner()) == ["STATUS"]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "после".into(),
        })
        .await;
    new.until("the reply in both", |f| {
        f.layout(owner()).contains(&"после".to_owned())
            && f.layout(Chat::Group).contains(&"после".to_owned())
    })
    .await;
    new.say(owner(), "из лички");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из лички", "private")
    );
    new.until("the echo in the group", |f| {
        f.layout(Chat::Group).contains(&echo("из лички"))
    })
    .await;
    let second = agent.next_within(Duration::from_millis(800)).await;
    assert!(
        !matches!(second, Some(HubMsg::Inbound { .. })),
        "a second inbound: {second:?}"
    );
    new.say(Chat::Group, "из группы");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из группы", "group")
    );
    new.until("the echo in the private chat, one status in each", |f| {
        f.layout(owner()) == ["после", "из лички", echo("из группы").as_str(), "STATUS"]
            && f.layout(Chat::Group)
                == [
                    "до",
                    "после",
                    echo("из лички").as_str(),
                    "из группы",
                    "STATUS",
                ]
    })
    .await;
}

/// The user deletes the private topic: the next message finds it gone and
/// the slot gets a new private topic, which takes the session again; that
/// message goes again into it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_deleted_private_topic_is_made_again() {
    let hub = start_hub("deleted", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    let first = hub.fake.topic(owner()).unwrap();
    hub.fake.delete_topic(owner());
    agent
        .send(AgentMsg::Reply {
            text: "раз".into()
        })
        .await;
    hub.until("a new private topic", |fake| {
        fake.topic(owner()).is_some_and(|topic| topic != first)
            && fake.layout(owner()).contains(&"STATUS".to_owned())
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "два".into()
        })
        .await;
    hub.until("both replies in the new topic, in order", |fake| {
        fake.layout(owner()) == ["раз", "два", "STATUS"]
    })
    .await;
}

/// Without topics in private chats the hub works as before: the group
/// only, and nothing ever goes to a private chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_without_topics_in_private_chats_the_group_is_all() {
    let hub = start_hub("off", Mode::Group, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the group", |fake| {
        fake.layout(Chat::Group) == ["STATUS"]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "привет".into(),
        })
        .await;
    hub.until("the reply in the group", |fake| {
        fake.layout(Chat::Group) == ["привет", "STATUS"]
    })
    .await;
    assert!(
        hub.fake
            .ops()
            .iter()
            .all(|op| op.chat().is_none_or(|chat| chat == Chat::Group)),
        "{:#?}",
        hub.fake.ops()
    );
    // The poll ignores the private chat then (`updates`): route_batch
    // without private chats.
    let allowlist = [OWNER].into_iter().collect();
    let update = serde_json::json!({ "update_id": 1, "message": {
        "message_id": 3, "date": 1, "text": "hi",
        "from": { "id": OWNER, "is_bot": false, "first_name": "x" },
        "chat": { "id": OWNER, "type": "private", "first_name": "x" },
    }});
    let (_, routed) = updates::route_batch(vec![update], None, -1001, &allowlist);
    assert_eq!(
        routed,
        [updates::Routed::Ignored(updates::Ignored::PrivateChat)]
    );
    let _ = Place::new(Chat::Group, None);
}

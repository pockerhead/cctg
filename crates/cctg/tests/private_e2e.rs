//! The owner's private view end to end (TASK-063): an agent over a real TCP
//! link to the real `serve_agents` (a hand-written client, like
//! `status_e2e.rs`), hook events straight into the real `Slots` actor, the
//! real per-chat `Outbox`, and a fake Bot API that keeps every chat on its
//! own: topics and messages are numbered per chat, as Telegram does, so a
//! message of the group and its twin in the private chat have different
//! ids. Private chats are on (`Options::owners`) with one owner. Sharing a
//! slot to the group and taking it out (TASK-064), then the menu in the
//! private chat's General (TASK-073), its compact turn view (TASK-076) and
//! the display of each view by its own settings (TASK-078), at the end.
//! Then a shared slot's group topic that answers mentions only (TASK-077).
//! Last, a second group joined while the hub runs, shared into through the
//! group picker, with its own budget (TASK-069).

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ChatInfo, ChatMember, ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat, Place, PrivateChat};
use cctg::hub::groups::{self, GroupLookup, KnownGroups};
use cctg::hub::ingress::{bind, serve_agents};
use cctg::hub::mention;
use cctg::hub::menu;
use cctg::hub::registry::{ICON_ALIVE, ICON_DEAD, RegistryStore, share_line};
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{
    ANSWER_PICKER, Control, ECHO_MARK, FALLBACK_END_NOTICE, FOREIGN_TOPIC_NOTICE, MAX_TWIN_POSTS,
    MIRROR_GAP_NOTICE, MOVED_MARK, MentionBot, Options, Owners, PRIVATE_CLOSED_NOTICE,
    PRIVATE_GENERAL_NOTICE, PRIVATE_START_TEXT, SHARE_OWNER_ONLY_NOTICE, SHARED_NOTICE, Slots,
    UNSHARED_KEPT_NOTICE, UNSHARED_NOTICE,
};
use cctg::hub::status;
use cctg::hub::updates::{CallbackInput, ConnectInput, Inbound, MemberUpdate};
use cctg::hub::{permissions, updates};
use cctg::wire::{
    self, AgentMsg, Behavior, HookEvent, HookPost, HubMsg, PermissionRequest, Register, Secret,
    SessionAnswer, SessionAsk, StreamItem, StreamLine,
};
use serde_json::Value;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The default group of this test (TASK-069): the chat its Bot API fake
/// and its registry name.
const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));

/// The groups the poll lets through: the default one.
fn known_groups() -> KnownGroups {
    KnownGroups::of([GroupChat::of(GROUP_ID)])
}

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
    /// The HTML it was last sent or written with (TASK-076).
    html: Option<String>,
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
    general: Vec<Shown>,
    /// The pinned message (TASK-073: the menu).
    pinned: Option<i64>,
}

#[derive(Default)]
struct Fake {
    chats: Mutex<HashMap<Chat, ChatModel>>,
    ops: Mutex<Vec<Op>>,
    /// Every call into a private chat is refused with 403 (no Start yet).
    forbid_private: AtomicBool,
    /// `deleteForumTopic` is refused: the bot may not delete messages.
    refuse_topic_delete: AtomicBool,
    /// HTML with a quote is refused as markup Telegram cannot parse
    /// (TASK-076).
    refuse_quote: AtomicBool,
    /// Every call into this chat gets a 429 until the test clears it
    /// (TASK-069).
    flood: Mutex<Option<Chat>>,
    /// `getChatMember` of a group says the bot may not manage topics
    /// (TASK-069).
    no_rights: AtomicBool,
}

/// The fake's answers about groups (TASK-069): a forum supergroup, the bot
/// an administrator with every right unless `no_rights`.
impl GroupLookup for Fake {
    async fn member(&self, _: GroupChat) -> Result<ChatMember, ApiError> {
        let rights = !self.no_rights.load(Ordering::SeqCst);
        Ok(ChatMember {
            status: "administrator".into(),
            can_manage_topics: rights,
            can_delete_messages: true,
            ..ChatMember::default()
        })
    }

    async fn info(&self, _: GroupChat) -> Result<ChatInfo, ApiError> {
        Ok(ChatInfo {
            kind: "supergroup".into(),
            title: Some(B_TITLE.into()),
            is_forum: true,
        })
    }

    async fn leave(&self, _: GroupChat) -> Result<(), ApiError> {
        Ok(())
    }
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
            .chain(self.general.iter_mut())
            .find(|shown| shown.id == id)
    }

    fn post(&mut self, base: i64, thread: Option<i64>, shown: Shown) -> Result<i64, ApiError> {
        self.next_message = self.next_message.max(base) + 1;
        let id = self.next_message;
        let Some(thread) = thread else {
            self.general.push(Shown { id, ..shown });
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
        if *self.flood.lock().unwrap() == Some(chat) {
            return Err(ApiError::RetryAfter(Duration::from_secs(5)));
        }
        if chat.is_private() && self.forbid_private.load(Ordering::SeqCst) {
            return Err(ApiError::Telegram {
                code: 403,
                description: "Forbidden: bot can't initiate conversation with a user".into(),
            });
        }
        if self.refuse_quote.load(Ordering::SeqCst)
            && let Op::Send {
                html: Some(html), ..
            }
            | Op::Stream {
                html: Some(html), ..
            } = op
            && html.contains("<blockquote")
        {
            return Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: can't parse entities: unsupported start tag".into(),
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
                html,
                reply_markup,
                permission,
                notify,
                ..
            } => {
                let shown = Shown {
                    id: 0,
                    text: text.clone(),
                    html: html.clone(),
                    buttons: buttons(reply_markup.as_ref()),
                    loud: *notify,
                    status: reply_markup.is_some() && !permission,
                };
                model.post(base, *thread_id, shown).and_then(sent)
            }
            Op::Stream {
                thread_id,
                text,
                html,
                notify,
                into: None,
                ..
            } => {
                let shown = Shown {
                    id: 0,
                    text: text.clone(),
                    html: html.clone(),
                    buttons: Vec::new(),
                    loud: *notify,
                    status: false,
                };
                model.post(base, Some(*thread_id), shown).and_then(sent)
            }
            Op::Stream {
                text,
                html,
                into: Some(id),
                ..
            } => match model.message(*id) {
                Some(shown) => {
                    shown.text.clone_from(text);
                    shown.html.clone_from(html);
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
                    shown.html = None;
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
                model.general.retain(|shown| shown.id != *message_id);
                Ok(Outcome::Done)
            }
            Op::Pin { message_id, .. } => {
                model.pinned = Some(*message_id);
                Ok(Outcome::Done)
            }
            Op::Unpin { message_id, .. } => {
                if model.pinned == Some(*message_id) {
                    model.pinned = None;
                }
                Ok(Outcome::Done)
            }
            Op::DeleteTopic { thread_id, .. } => {
                if self.refuse_topic_delete.load(Ordering::SeqCst) {
                    return Err(ApiError::Telegram {
                        code: 400,
                        description: "Bad Request: not enough rights to delete a topic".into(),
                    });
                }
                model.topics.remove(thread_id);
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
            .map(|model| model.general.iter().map(|m| m.text.clone()).collect())
            .unwrap_or_default()
    }

    /// The pinned message of `chat`'s General (TASK-073: the menu).
    fn menu(&self, chat: Chat) -> Option<Shown> {
        let chats = self.chats.lock().unwrap();
        let model = chats.get(&chat)?;
        let pinned = model.pinned?;
        model.general.iter().find(|m| m.id == pinned).cloned()
    }

    /// The edits of message `message_id` of `chat` so far.
    fn edits_of(&self, chat: Chat, message_id: i64) -> usize {
        self.ops()
            .iter()
            .filter(|op| {
                matches!(op, Op::Edit { chat: to, message_id: id, .. } if *to == chat && *id == message_id)
            })
            .count()
    }

    /// The texts of the answers to button presses so far.
    fn answers(&self) -> Vec<Option<String>> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::AnswerCallback { text, .. } => Some(text),
                _ => None,
            })
            .collect()
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
            html: None,
            buttons: Vec::new(),
            loud: false,
            status: false,
        };
        model.post(base, thread, shown).expect("the topic is there")
    }

    /// How many topics `chat` has.
    fn topics(&self, chat: Chat) -> usize {
        let chats = self.chats.lock().unwrap();
        chats.get(&chat).map_or(0, |model| model.topics.len())
    }

    /// The id the next message of `chat` gets.
    fn next_id(&self, chat: Chat) -> i64 {
        let base = if chat.is_private() { 5000 } else { 1000 };
        let mut chats = self.chats.lock().unwrap();
        let model = chats.entry(chat).or_default();
        model.next_message = model.next_message.max(base) + 1;
        model.next_message
    }

    /// The status message `chat` shows.
    fn status(&self, chat: Chat) -> Option<Shown> {
        self.shown(chat)
            .into_iter()
            .rev()
            .find(|shown| shown.status)
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
    /// The groups the poll would let through (TASK-069).
    groups: KnownGroups,
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
const SHARED: Mode = Mode::Private {
    share_new: true,
    menu: false,
    mentions: false,
};
/// Private chats on as the hub runs: a new slot shows in the private chat
/// alone (decision 2026-09-28).
const PRIVATE: Mode = Mode::Private {
    share_new: false,
    menu: false,
    mentions: false,
};
/// [`PRIVATE`] with the menu (TASK-073), as the hub runs.
const MENU: Mode = Mode::Private {
    share_new: false,
    menu: true,
    mentions: false,
};
/// [`MENU`] with the bot known by its name: a shared slot's group topic
/// answers mentions only (TASK-077), as the hub runs.
const MENTIONS: Mode = Mode::Private {
    share_new: false,
    menu: true,
    mentions: true,
};
/// The bot of [`MENTIONS`].
const BOT_ID: i64 = 8_100_200_300;
const BOT: &str = "cctg_test_bot";

#[derive(Clone, Copy)]
enum Mode {
    /// The bot has no topics in private chats.
    Group,
    Private {
        share_new: bool,
        menu: bool,
        mentions: bool,
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
    let registry = store.load(GroupChat::of(GROUP_ID)).unwrap();
    let groups = KnownGroups::default();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(Duration::from_millis(50)),
        owners: match mode {
            Mode::Group => None,
            Mode::Private { share_new, .. } => Some(Owners {
                first: PrivateChat::of_user(OWNER),
                devices: None,
                share_new,
            }),
        },
        menu: matches!(mode, Mode::Private { menu: true, .. }),
        mentions: matches!(mode, Mode::Private { mentions: true, .. }).then(|| MentionBot {
            id: BOT_ID,
            username: BOT.into(),
        }),
        groups: groups.clone(),
        ..Options::default()
    };
    let mut slots = Slots::new(registry, store, outbox, options);
    slots.look_up_groups(fake.clone());
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
        groups,
        agent_addr,
        hooks,
        control,
        tasks,
        _state: TempRoot(keep),
    }
}

impl Hub {
    async fn hook(&self, event: HookEvent) {
        self.hook_of(SESSION, event).await;
    }

    async fn hook_of(&self, session: &str, event: HookEvent) {
        let post = HookPost::new(
            HOST.into(),
            session.into(),
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

    /// [`Hub::start`] with a transcript path (TASK-076): the session is
    /// streamed through its agent, the hub never reads the file itself.
    async fn start_streamed(&self) {
        let path = self._state.0.join("never-written.jsonl");
        let post = HookPost::new(
            HOST.into(),
            SESSION.into(),
            CWD.into(),
            path.display().to_string(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(4242),
                parent_claude_pid: None,
            },
        );
        self.hooks.send(post).await.unwrap();
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
                self.fake.shown(GROUP)
            );
        }
    }

    /// Waits until both views show `want`.
    async fn both(&self, what: &str, want: &[&str]) {
        self.until(what, |fake| {
            fake.layout(owner()) == want && fake.layout(GROUP) == want
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
                from_username: false,
                author: Some(NAME.into()),
                display_name: Some(NAME.into()),
                reply_from: None,
            }))
            .unwrap();
        message_id
    }

    /// The owner's message in topic `thread` of `chat`, which the fake need
    /// not have (a deleted topic): handed over as the poll does.
    fn say_in(&self, chat: Chat, thread: i64, text: &str) {
        self.say_as(OWNER, chat, thread, text);
    }

    /// A message of allowlisted user `user` in topic `thread` of `chat`.
    fn say_as(&self, user: i64, chat: Chat, thread: i64, text: &str) {
        self.control
            .send(Control::Message(Inbound {
                chat,
                sender: PrivateChat::of_user(user),
                message_id: self.fake.next_id(chat),
                thread_id: Some(thread),
                text: Some(text.into()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
                from_name: None,
                from_username: false,
                author: Some(NAME.into()),
                display_name: Some(NAME.into()),
                reply_from: None,
            }))
            .unwrap();
    }

    /// Team member `name`'s message in the group topic, an explicit reply to
    /// a message of sender `reply_from` when given (TASK-077); its id.
    fn say_group(&self, name: &str, text: &str, reply_from: Option<i64>) -> i64 {
        let message_id = self.fake.user(GROUP, text);
        self.control
            .send(Control::Message(Inbound {
                chat: GROUP,
                sender: PrivateChat::of_user(OWNER),
                message_id,
                thread_id: self.fake.topic(GROUP),
                text: Some(text.into()),
                reply_to: reply_from.map(|_| message_id - 1),
                quote: None,
                forwarded: false,
                media: None,
                from_name: Some(name.into()),
                from_username: false,
                author: Some(name.into()),
                display_name: Some(name.into()),
                reply_from,
            }))
            .unwrap();
        message_id
    }

    /// A user's message in the General of `chat`.
    fn say_general(&self, chat: Chat, text: &str) {
        self.control
            .send(Control::Message(Inbound {
                display_name: None,
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
                from_username: false,
                author: None,
                reply_from: None,
            }))
            .unwrap();
    }

    /// A press of the button `data` of message `message_id` in the General
    /// of `chat` (TASK-073: the menu).
    fn press_general(&self, chat: Chat, message_id: i64, data: &str) {
        self.control
            .send(Control::Callback(CallbackInput {
                chat: Some(chat),
                query_id: format!("q-{data}-{message_id}"),
                data: Some(data.into()),
                message_id: Some(message_id),
                thread_id: None,
                from_name: None,
                display_name: Some(NAME.into()),
                sender: PrivateChat::of_user(OWNER),
            }))
            .unwrap();
    }

    /// Presses `data` on the owner's menu and waits for its one edit, and a
    /// little more for a second one that must not come.
    async fn menu_press(&self, data: &str) {
        let menu = self.fake.menu(owner()).expect("a menu").id;
        let before = self.fake.edits_of(owner(), menu);
        self.press_general(owner(), menu, data);
        self.until(data, |fake| fake.edits_of(owner(), menu) > before)
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(self.fake.edits_of(owner(), menu), before + 1, "{data}");
    }

    /// Stops the hub as an update or a restart does (`Control::Stop`): it
    /// returns once the slot actor has its last registry snapshot on disk.
    /// Dropping it instead cuts the actor off: a snapshot still being
    /// written (a slow disk) leaves an older `registry.json` behind.
    async fn stop(mut self) {
        self.control.send(Control::Stop).unwrap();
        let slots = self.tasks.pop().expect("the slot actor");
        tokio::time::timeout(WAIT, slots)
            .await
            .expect("the hub stops")
            .unwrap();
    }

    /// The state directory's `registry.json`, as JSON.
    fn saved(&self) -> Value {
        let bytes = std::fs::read(self._state.0.join("registry.json")).unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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
                display_name: Some(NAME.into()),
                sender: PrivateChat::of_user(OWNER),
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
        Self::connect_as(hub, private_place, false).await
    }

    /// `console_keys`: it writes ⏹ into the console (TASK-073 menu tests).
    /// It returns once the hub bound it to its session: `registered` only
    /// says the link took the frame, and a message written before the slot
    /// actor bound the agent waits in the buffer with a notice, as it would
    /// for a real session whose agent is not up yet.
    async fn connect_as(hub: &Hub, private_place: bool, console_keys: bool) -> Self {
        Self::connect_with(hub, private_place, console_keys, false, false).await
    }

    /// An agent in the private chat that reads transcripts (TASK-076): a
    /// task of its own answers every read from `transcript`.
    async fn serve(hub: &Hub, transcript: Transcript) -> JoinHandle<()> {
        let mut agent = Self::connect_with(hub, true, false, true, false).await;
        tokio::spawn(async move {
            while let Some(msg) = agent.next_within(Duration::from_secs(3600)).await {
                if let HubMsg::TranscriptRead {
                    session_id, from, ..
                } = msg
                {
                    let chunk = transcript.chunk(session_id, from);
                    agent.send(chunk).await;
                }
            }
        })
    }

    /// `session_reads`: it is asked for session reads (TASK-077: the
    /// compression of a group history).
    async fn connect_with(
        hub: &Hub,
        private_place: bool,
        console_keys: bool,
        transcript_reads: bool,
        session_reads: bool,
    ) -> Self {
        let from = hub.fake.ops().len();
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
            transcript_reads,
            console_keys,
            console_commands: false,
            console_line_chars: 0,
            client: None,
            files: false,
            session_reads,
            status_lines: false,
            private_place,
            enrolled: None,
            heartbeat: false,
            sandbox: None,
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
        hub.until("the agent bound: the alive icon", |fake| {
            alive_since(fake, from)
        })
        .await;
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

/// A topic call after call `from` gives a topic the alive icon: the slot's
/// session has its agent bound (the icon shows "no channel" until then).
fn alive_since(fake: &Fake, from: usize) -> bool {
    fake.ops()[from..].iter().any(|op| {
        matches!(op,
            Op::CreateTopic { icon_custom_emoji_id: Some(icon), .. }
            | Op::EditTopic { icon_custom_emoji_id: Some(icon), .. } if icon == ICON_ALIVE)
    })
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
            .all(|op| op.chat().is_none_or(|chat| chat != GROUP)),
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
    for chat in [owner(), GROUP] {
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
    let group_ids: Vec<i64> = hub.fake.shown(GROUP).iter().map(|m| m.id).collect();
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
        fake.layout(GROUP).contains(&echo("из лички"))
    })
    .await;

    let group_id = hub.say(GROUP, "из группы");
    let (content, meta) = agent.inbound().await;
    assert_eq!(content, "из группы");
    assert_eq!(meta["place"], "group");
    assert_eq!(meta["message_id"], group_id.to_string());
    // Each view shows what was written in the other, signed, and the
    // status goes below it all in both.
    hub.until("the echoes, and the status below them", |fake| {
        fake.layout(owner()) == ["из лички".to_owned(), echo("из группы"), "STATUS".into()]
            && fake.layout(GROUP) == [echo("из лички"), "из группы".to_owned(), "STATUS".into()]
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

    for (request_id, pressed, other) in [("abcde", GROUP, owner()), ("fghij", owner(), GROUP)] {
        agent.send(permission(request_id)).await;
        hub.until("the prompt in both views", |fake| {
            [owner(), GROUP].into_iter().all(|chat| {
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
            [owner(), GROUP].into_iter().all(|chat| {
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
        for chat in [owner(), GROUP] {
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
    hub.say(GROUP, "из группы");
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
        fake.layout(GROUP) == ["STATUS"] && fake.general(GROUP) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "только в группе".into(),
        })
        .await;
    hub.until("the reply in the group", |fake| {
        fake.layout(GROUP) == ["только в группе", "STATUS"]
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
        fake.layout(GROUP) == ["только в группе", FALLBACK_END_NOTICE]
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
        hub.fake.layout(GROUP),
        ["только в группе", FALLBACK_END_NOTICE],
        "nothing more in the group"
    );
    assert_eq!(
        hub.fake.general(GROUP),
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
        fake.layout(GROUP) == ["в заблокированную", "STATUS"]
            && fake.general(GROUP) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    assert_eq!(hub.fake.layout(owner()), ["STATUS"], "the old status stays");

    // The owner writes in the private topic: it opens again.
    hub.fake.forbid_private.store(false, Ordering::SeqCst);
    hub.say(owner(), "снова тут");
    let (content, _) = agent.inbound().await;
    assert_eq!(content, "снова тут");
    hub.until("the slot leaves the group", |fake| {
        fake.layout(GROUP) == ["в заблокированную", FALLBACK_END_NOTICE]
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
                && asks_in(fake, GROUP, "abcde")
        },
    )
    .await;
    assert_eq!(prompts_in(&hub.fake, GROUP), 1, "the twin is kept");
    allow_in(&hub, &mut agent, owner(), "abcde").await;
    hub.until("both show the decision", |fake| {
        allowed_in(fake, owner(), "abcde") && allowed_in(fake, GROUP, "abcde")
    })
    .await;
    assert_eq!(prompts_in(&hub.fake, GROUP), 1);
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
            .all(|op| op.chat().is_none_or(|chat| chat != GROUP)),
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
        asks_in(fake, GROUP, "abcde") && fake.general(GROUP) == [PRIVATE_CLOSED_NOTICE]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(prompts_in(&hub.fake, GROUP), 1, "not sent twice");
    allow_in(&hub, &mut agent, GROUP, "abcde").await;
    hub.until("the group shows the decision", |fake| {
        allowed_in(fake, GROUP, "abcde")
    })
    .await;
    assert_eq!(prompts_in(&hub.fake, GROUP), 1);
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
    old.until("status in the group", |f| f.layout(GROUP) == ["STATUS"])
        .await;
    agent
        .send(AgentMsg::Reply {
            text: "до".into()
        })
        .await;
    old.until("reply in the group", |f| {
        f.layout(GROUP) == ["до", "STATUS"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(agent);
    old.stop().await;
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
            && f.layout(GROUP).contains(&"после".to_owned())
    })
    .await;
    new.say(owner(), "из лички");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из лички", "private")
    );
    new.until("the echo in the group", |f| {
        f.layout(GROUP).contains(&echo("из лички"))
    })
    .await;
    let second = agent.next_within(Duration::from_millis(800)).await;
    assert!(
        !matches!(second, Some(HubMsg::Inbound { .. })),
        "a second inbound: {second:?}"
    );
    new.say(GROUP, "из группы");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("из группы", "group")
    );
    new.until("the echo in the private chat, one status in each", |f| {
        f.layout(owner()) == ["после", "из лички", echo("из группы").as_str(), "STATUS"]
            && f.layout(GROUP)
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
        fake.layout(GROUP) == ["STATUS"]
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "привет".into(),
        })
        .await;
    hub.until("the reply in the group", |fake| {
        fake.layout(GROUP) == ["привет", "STATUS"]
    })
    .await;
    assert!(
        hub.fake
            .ops()
            .iter()
            .all(|op| op.chat().is_none_or(|chat| chat == GROUP)),
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
    let (_, routed) = updates::route_batch(vec![update], None, &known_groups(), &allowlist);
    assert_eq!(
        routed,
        [updates::Routed::Ignored(updates::Ignored::PrivateChat)]
    );
    let _ = Place::new(GROUP, None);
}

// ---------------------------------------------------------------- TASK-064

/// A call that puts something new into the group: a message or a topic.
fn posts_to_group(op: &Op) -> bool {
    matches!(
        op,
        Op::Send { chat: GROUP, .. }
            | Op::Stream { chat: GROUP, .. }
            | Op::CreateTopic { chat: GROUP, .. }
    )
}

/// The session's status message in `chat` has a button labelled `label`.
fn status_has(fake: &Fake, chat: Chat, label: &str) -> bool {
    fake.status(chat)
        .is_some_and(|status| status.buttons.iter().any(|button| button == label))
}

/// A session in the private chat alone, shared with `/share`: the group
/// topic shows the share line, then the status.
async fn shared_hub(name: &str, fake: Fake) -> (Hub, Agent) {
    let hub = start_hub(name, PRIVATE, fake).await;
    hub.start().await;
    let agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.say(owner(), "/share");
    let line = share_line(NAME);
    hub.until("the group topic: its line, then the status", |fake| {
        fake.layout(GROUP) == [line.as_str(), "STATUS"]
    })
    .await;
    // The fake shows the group's status message a moment before the hub has
    // Telegram's answer about it; an unshare that keeps the topic takes away
    // only a status message the hub knows.
    hub.until("the hub knows the group's status message", |fake| {
        let status = fake.status(GROUP).map(|shown| shown.id);
        status.is_some() && group_status(&hub.saved()) == status
    })
    .await;
    (hub, agent)
}

/// The status message of the group view in a saved `registry.json`.
fn group_status(saved: &Value) -> Option<i64> {
    saved["slots"][0]["views"]
        .as_array()?
        .iter()
        .find(|view| view["chat"] == serde_json::json!({ "group": GROUP_ID }))?["status"]
        ["message_id"]
        .as_i64()
}

/// `/share` in the private topic: a group topic that starts with the share
/// line, without what came before; from then on the session shows in both,
/// and the group writes to it as in a shared slot. The answer stays in the
/// private chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_share_opens_a_group_topic_with_its_line_and_mirrors_only_what_comes_after() {
    let hub = start_hub("share", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.say(owner(), "раз");
    let (content, _) = agent.inbound().await;
    assert_eq!(content, "раз");
    assert!(
        hub.fake
            .ops()
            .iter()
            .all(|op| op.chat().is_none_or(|chat| chat != GROUP)),
        "nothing in the group before the share"
    );
    hub.say(owner(), "/share");
    let line = share_line(NAME);
    assert_eq!(line, "── общий доступ: Анна ──");
    hub.until("the group topic, the answer in the private chat", |fake| {
        fake.layout(GROUP) == [line.as_str(), "STATUS"]
            && fake.layout(owner()).contains(&SHARED_NOTICE.to_owned())
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "ответ".into(),
        })
        .await;
    hub.until("the reply in both", |fake| {
        fake.layout(owner()).contains(&"ответ".to_owned())
            && fake.layout(GROUP) == [line.as_str(), "ответ", "STATUS"]
    })
    .await;
    // The group writes to the session; the command before never reached it.
    hub.say(GROUP, "два");
    let (content, meta) = agent.inbound().await;
    assert_eq!((content.as_str(), meta["place"].as_str()), ("два", "group"));
    hub.until("the echo in the private chat", |fake| {
        fake.layout(owner()).contains(&echo("два"))
    })
    .await;
    let group = hub.fake.layout(GROUP);
    assert!(
        !group.contains(&"раз".to_owned()) && !group.contains(&SHARED_NOTICE.to_owned()),
        "{group:?}"
    );
    assert!(
        hub.fake
            .status(GROUP)
            .is_some_and(|status| status.buttons.is_empty()),
        "the twin has no share button"
    );
}

/// `/unshare` deletes the group topic; the session goes on in the private
/// chat alone, with one status message there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_unshare_deletes_the_group_topic_and_the_session_goes_on_in_private() {
    let (hub, mut agent) = shared_hub("unshare", Fake::default()).await;
    let thread = hub.fake.topic(GROUP).unwrap();
    hub.say(owner(), "/unshare");
    hub.until("the group topic gone, the private chat told", |fake| {
        let private = fake.layout(owner());
        fake.topics(GROUP) == 0
            && private.contains(&UNSHARED_NOTICE.to_owned())
            && private.iter().filter(|text| *text == "STATUS").count() == 1
            && private.last().is_some_and(|text| text == "STATUS")
    })
    .await;
    assert!(hub.fake.ops().iter().any(|op| matches!(
        op,
        Op::DeleteTopic { chat: GROUP, thread_id } if *thread_id == thread
    )));
    let after = hub.fake.ops().len();
    agent
        .send(AgentMsg::Reply {
            text: "после".into(),
        })
        .await;
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some("Готово".into()),
    })
    .await;
    hub.until("the reply and the answer in the private chat", |fake| {
        let private = fake.layout(owner());
        private.contains(&"после".to_owned()) && private.contains(&"Готово".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = hub.fake.ops();
    assert!(
        !ops[after..].iter().any(posts_to_group),
        "{:#?}",
        &ops[after..]
    );
}

/// The status button: «👥 В группу» shares, «🙈 Убрать из группы» asks
/// again, and the confirming press takes the slot out of the group.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_the_status_button_shares_and_unshares_after_a_confirm() {
    let hub = start_hub("share-button", PRIVATE, Fake::default()).await;
    hub.start().await;
    let _agent = Agent::connect(&hub, true).await;
    hub.until("the share button on the private status", |fake| {
        status_has(fake, owner(), status::SHARE_BUTTON)
    })
    .await;
    assert_eq!(
        press_status(&hub, "status:share").await,
        status::ANSWER_SHARED
    );
    let line = share_line(NAME);
    hub.until("the group topic, the unshare button", |fake| {
        fake.layout(GROUP) == [line.as_str(), "STATUS"]
            && status_has(fake, owner(), status::UNSHARE_BUTTON)
    })
    .await;
    assert_eq!(
        press_status(&hub, "status:unshare").await,
        status::ANSWER_UNSHARE_CONFIRM
    );
    hub.until("the confirming button", |fake| {
        status_has(fake, owner(), status::UNSHARE_CONFIRM_BUTTON)
    })
    .await;
    assert_eq!(hub.fake.topics(GROUP), 1, "not before the confirm");
    assert_eq!(
        press_status(&hub, "status:unshare_confirm").await,
        status::ANSWER_UNSHARED
    );
    hub.until("the group topic gone, the share button back", |fake| {
        fake.topics(GROUP) == 0 && status_has(fake, owner(), status::SHARE_BUTTON)
    })
    .await;
}

/// Presses `data` on the private status message and returns the answer.
/// The fake shows a moved status message a moment before the hub has
/// Telegram's answer about it; a press then is answered as stale (as it
/// would be in Telegram) and is made again.
async fn press_status(hub: &Hub, data: &str) -> String {
    let answers = |fake: &Fake| -> Vec<String> {
        fake.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::AnswerCallback { text, .. } => text,
                _ => None,
            })
            .collect()
    };
    for _ in 0..100 {
        let before = answers(&hub.fake).len();
        let status = hub.fake.status(owner()).unwrap().id;
        hub.press(owner(), status, data);
        hub.until("the press is answered", |fake| answers(fake).len() > before)
            .await;
        let answer = answers(&hub.fake).remove(before);
        if answer != status::ANSWER_STALE {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{data}: the status message stays unknown to the hub");
}

/// Only the owner shares and unshares, from their private topic: another
/// user's private chat with the same topic number gets the foreign-topic
/// answer, `/unshare` in the group is refused there, and a press on the
/// twin of the status message in the group does nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_only_the_owner_shares_and_unshares() {
    const OTHER: i64 = 7_319_402_519;
    let hub = start_hub("owner-only", PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    let other = Chat::Private(PrivateChat::of_user(OTHER));
    let thread = hub.fake.topic(owner()).unwrap();
    hub.say_as(OTHER, other, thread, "/share");
    hub.until("the other user is told where to write", |fake| {
        fake.ops().iter().any(|op| {
            matches!(op, Op::Send { chat, text, .. } if *chat == other && text == FOREIGN_TOPIC_NOTICE)
        })
    })
    .await;
    assert_eq!(hub.fake.topics(GROUP), 0);
    assert!(!hub.fake.ops().iter().any(posts_to_group));

    hub.say(owner(), "/share");
    let line = share_line(NAME);
    hub.until("the group topic", |fake| {
        fake.layout(GROUP) == [line.as_str(), "STATUS"]
    })
    .await;
    hub.say(GROUP, "/unshare");
    hub.until("the group is told who may", |fake| {
        fake.layout(GROUP)
            .contains(&SHARE_OWNER_ONLY_NOTICE.to_owned())
    })
    .await;
    assert!(
        !matches!(
            agent.next_within(Duration::from_millis(300)).await,
            Some(HubMsg::Inbound { .. })
        ),
        "the command reaches no session"
    );
    let twin = hub.fake.status(GROUP).unwrap().id;
    hub.press(GROUP, twin, "status:unshare_confirm");
    hub.until("the press is refused", |fake| {
        fake.ops().iter().any(|op| {
            matches!(op, Op::AnswerCallback { text: Some(text), .. } if text == status::ANSWER_OWNER_ONLY)
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hub.fake.topics(GROUP), 1);
    assert!(
        !hub.fake
            .ops()
            .iter()
            .any(|op| matches!(op, Op::DeleteTopic { .. }))
    );
}

/// A prompt open in both views as the slot is unshared stays decidable in
/// the private chat; the group gets no topic again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_an_unshare_with_an_open_prompt_leaves_it_decidable_in_private() {
    let (hub, mut agent) = shared_hub("unshare-prompt", Fake::default()).await;
    agent.send(permission("abcde")).await;
    hub.until("the prompt in both views", |fake| {
        asks_in(fake, owner(), "abcde") && asks_in(fake, GROUP, "abcde")
    })
    .await;
    hub.say(owner(), "/unshare");
    hub.until("the group topic gone", |fake| fake.topics(GROUP) == 0)
        .await;
    let after = hub.fake.ops().len();
    allow_in(&hub, &mut agent, owner(), "abcde").await;
    hub.until("the private prompt shows the decision", |fake| {
        allowed_in(fake, owner(), "abcde")
    })
    .await;
    let ops = hub.fake.ops();
    assert!(
        !ops[after..]
            .iter()
            .any(|op| matches!(op, Op::CreateTopic { chat: GROUP, .. })),
        "{:#?}",
        &ops[after..]
    );
}

/// A message written into the group topic as it is being deleted reaches
/// no session and gets no answer; the session goes on in the private chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_messages_written_into_the_group_as_it_is_unshared_go_nowhere_quietly() {
    let (hub, mut agent) = shared_hub("unshare-late", Fake::default()).await;
    let thread = hub.fake.topic(GROUP).unwrap();
    hub.say(owner(), "/unshare");
    hub.until("the group topic gone", |fake| fake.topics(GROUP) == 0)
        .await;
    let after = hub.fake.ops().len();
    hub.say_in(GROUP, thread, "поздно");
    assert!(
        !matches!(
            agent.next_within(Duration::from_millis(300)).await,
            Some(HubMsg::Inbound { .. })
        ),
        "no inbound from a topic that is no view"
    );
    agent
        .send(AgentMsg::Reply {
            text: "дальше".into(),
        })
        .await;
    hub.until("the reply in the private chat", |fake| {
        fake.layout(owner()).contains(&"дальше".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ops = hub.fake.ops();
    assert!(
        !ops[after..].iter().any(posts_to_group),
        "{:#?}",
        &ops[after..]
    );
}

/// A dead slot can be shared: its group topic shows the line and the
/// ended status; the next session of the slot shows in both, after its
/// separator.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_dead_slot_can_be_shared_and_its_next_session_is_shared_too() {
    const NEXT: &str = "1b27f3f1-0000-4000-8000-000000000632";
    let hub = start_hub("share-dead", PRIVATE, Fake::default()).await;
    hub.start().await;
    let agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.hook(HookEvent::SessionEnd {
        reason: None,
        claude_pid: Some(4242),
    })
    .await;
    drop(agent);
    let ended = |fake: &Fake, chat: Chat| {
        fake.status(chat)
            .is_some_and(|status| status.text.starts_with("🏁"))
    };
    hub.until("the session ended", |fake| ended(fake, owner()))
        .await;
    hub.say(owner(), "/share");
    let line = share_line(NAME);
    hub.until(
        "the group topic with the line and the ended status",
        |fake| fake.layout(GROUP) == [line.as_str(), "STATUS"] && ended(fake, GROUP),
    )
    .await;
    hub.hook_of(
        NEXT,
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(4243),
            parent_claude_pid: None,
        },
    )
    .await;
    let separator = "── session 1b27f3f1 · new ──".to_owned();
    hub.until("the separator in both views", |fake| {
        fake.layout(owner()).contains(&separator) && fake.layout(GROUP).contains(&separator)
    })
    .await;
}

/// A slot in the group only because the owner blocked the bot: once the
/// owner is back, `/share` in the private topic keeps that group topic as
/// the shared one instead of leaving it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_share_keeps_the_fallback_group_topic() {
    let hub = start_hub("share-fallback", PRIVATE, Fake::default()).await;
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
    hub.until("the slot is in the group", |fake| {
        fake.layout(GROUP) == ["в заблокированную", "STATUS"]
    })
    .await;
    hub.fake.forbid_private.store(false, Ordering::SeqCst);
    hub.say(owner(), "/share");
    let line = share_line(NAME);
    hub.until("the share line in the old group topic", |fake| {
        fake.layout(GROUP).contains(&line)
            && fake.layout(owner()).contains(&SHARED_NOTICE.to_owned())
    })
    .await;
    let group = hub.fake.layout(GROUP);
    assert_eq!(hub.fake.topics(GROUP), 1, "the same topic");
    assert_eq!(group[0], "в заблокированную");
    assert!(
        !group.contains(&FALLBACK_END_NOTICE.to_owned()),
        "{group:?}"
    );
    agent
        .send(AgentMsg::Reply {
            text: "снова вместе".into(),
        })
        .await;
    hub.until("the reply in both", |fake| {
        fake.layout(owner()).contains(&"снова вместе".to_owned())
            && fake.layout(GROUP).contains(&"снова вместе".to_owned())
    })
    .await;
}

/// Sharing is kept over a hub restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_shared_slot_stays_shared_over_a_hub_restart() {
    let state = fresh_state("share-restart");
    let fake = Arc::new(Fake::default());
    let old = start_hub_on(&state, PRIVATE, fake.clone(), false).await;
    old.start().await;
    let agent = Agent::connect(&old, true).await;
    old.until("the status in the private chat", |f| {
        f.layout(owner()) == ["STATUS"]
    })
    .await;
    old.say(owner(), "/share");
    let line = share_line(NAME);
    old.until("the group topic", |f| {
        f.layout(GROUP) == [line.as_str(), "STATUS"]
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(agent);
    old.stop().await;
    let new = start_hub_on(&state, PRIVATE, fake.clone(), true).await;
    let mut agent = Agent::connect(&new, true).await;
    agent
        .send(AgentMsg::Reply {
            text: "после".into(),
        })
        .await;
    new.until("the reply in both", |f| {
        f.layout(owner()).contains(&"после".to_owned())
            && f.layout(GROUP).contains(&"после".to_owned())
    })
    .await;
    assert_eq!(fake.topics(GROUP), 1);
}

/// Without the right to delete messages the group topic stays: it is told,
/// gets the dead icon, loses its status twin and gets nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_without_the_delete_right_an_unshared_topic_is_told_and_left() {
    let fake = Fake::default();
    fake.refuse_topic_delete.store(true, Ordering::SeqCst);
    let (hub, mut agent) = shared_hub("unshare-kept", fake).await;
    let thread = hub.fake.topic(GROUP).unwrap();
    hub.say(owner(), "/unshare");
    hub.until("the group topic told, dead, without status", |fake| {
        let group = fake.layout(GROUP);
        group.contains(&UNSHARED_KEPT_NOTICE.to_owned())
            && !group.contains(&"STATUS".to_owned())
            && fake.ops().iter().any(|op| {
                matches!(op, Op::EditTopic { chat: GROUP, thread_id, icon_custom_emoji_id: Some(icon), .. }
                    if *thread_id == thread && icon == ICON_DEAD)
            })
    })
    .await;
    assert_eq!(hub.fake.topics(GROUP), 1);
    let after = hub.fake.ops().len();
    agent
        .send(AgentMsg::Reply {
            text: "дальше".into(),
        })
        .await;
    hub.until("the reply in the private chat", |fake| {
        fake.layout(owner()).contains(&"дальше".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ops = hub.fake.ops();
    assert!(
        !ops[after..].iter().any(posts_to_group),
        "{:#?}",
        &ops[after..]
    );
}

// ---------------------------------------------------------------- TASK-073

/// A hub with the menu, session started, its agent bound, the private
/// topic's status and the owner's pinned menu there.
async fn menu_hub(name: &str, mode: Mode, console_keys: bool) -> (Hub, Agent) {
    let hub = start_hub(name, mode, Fake::default()).await;
    hub.start().await;
    let agent = Agent::connect_as(&hub, true, console_keys).await;
    hub.until("the status and the pinned menu", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned())
            && fake
                .menu(owner())
                .is_some_and(|menu| menu.text.starts_with("Сессии (стр. 1/1)"))
    })
    .await;
    (hub, agent)
}

/// The first private topic brings the pinned menu; `/menu` and `/start`
/// put up a new one, pinned, and the old one and the command go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_start_shows_the_pinned_menu_and_menu_shows_it_again() {
    let (hub, _agent) = menu_hub("menu-start", MENU, false).await;
    let first = hub.fake.menu(owner()).unwrap();
    assert!(
        first.buttons.contains(&"· 🗂 Сессии".to_owned()),
        "{first:?}"
    );
    assert!(
        first.text.contains("1. ") && first.text.contains("[e2ebox] private · 0a16e2e0 — "),
        "{}",
        first.text
    );
    assert_eq!(hub.fake.general(owner()).len(), 1);
    hub.say_general(owner(), "/menu");
    hub.until("a new pinned menu, the old one gone", |fake| {
        fake.menu(owner()).is_some_and(|menu| menu.id != first.id)
            && fake.general(owner()).len() == 1
    })
    .await;
    // The command went too (its id: 1).
    assert!(hub.fake.ops().iter().any(|op| matches!(
        op,
        Op::Delete { chat, message_id: 1 } if *chat == owner()
    )));
    let second = hub.fake.menu(owner()).unwrap().id;
    hub.say_general(owner(), "/start");
    hub.until("a third menu", |fake| {
        fake.menu(owner()).is_some_and(|menu| menu.id != second) && fake.general(owner()).len() == 1
    })
    .await;
    assert!(
        !hub.fake
            .general(owner())
            .contains(&PRIVATE_START_TEXT.to_owned()),
        "the menu answers /start"
    );
    assert!(!texts(&hub.fake.ops()).contains(&OWNER.to_string()));
}

/// Each section press edits the one menu once; the settings are in
/// `registry.json` and hold after a restart: the menu still answers, and
/// the turn's answer in the private topic is quiet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_menu_sections_change_settings_and_they_survive_a_restart() {
    let state = fresh_state("menu-restart");
    let fake = Arc::new(Fake::default());
    let hub = start_hub_on(&state, MENU, fake.clone(), false).await;
    hub.start().await;
    let agent = Agent::connect(&hub, true).await;
    hub.until("the pinned menu", |fake| fake.menu(owner()).is_some())
        .await;
    hub.menu_press("menu:d").await;
    assert!(
        fake.menu(owner())
            .unwrap()
            .text
            .starts_with("Что показывать")
    );
    hub.menu_press("menu:dl:b").await;
    hub.menu_press("menu:th:0").await;
    let shown = fake.menu(owner()).unwrap();
    assert!(shown.buttons.contains(&"✅ Кратко".to_owned()), "{shown:?}");
    assert!(shown.buttons.contains(&"💭 Размышления: выкл".to_owned()));
    hub.menu_press("menu:n").await;
    hub.menu_press("menu:sn:o").await;
    // Quiet hours want the zone first.
    hub.menu_press("menu:q:1").await;
    assert!(
        fake.menu(owner())
            .unwrap()
            .text
            .starts_with("Сколько у вас")
    );
    hub.menu_press("menu:tz:180").await;
    hub.menu_press("menu:q:1").await;
    let shown = fake.menu(owner()).unwrap();
    assert!(shown.buttons.contains(&"✅ Ничего".to_owned()), "{shown:?}");
    assert!(
        shown
            .buttons
            .contains(&"🌙 Тихие часы: с 23 до 08".to_owned())
    );
    assert!(shown.buttons.contains(&"🕒 Пояс: UTC+3".to_owned()));
    let want = serde_json::json!({
        "detail": "brief", "thinking": false, "turn": "full", "sound": "off",
        "quiet": {"from": 23, "to": 8}, "tz": 180, "rich": true,
    });
    let file = state.join("registry.json");
    let saved = || -> Value {
        let bytes = std::fs::read(&file).unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let reached = async {
        while saved()["people"][0]["settings"] != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(WAIT, reached)
        .await
        .expect("the settings are saved");
    let menu = fake.menu(owner()).unwrap().id;
    assert_eq!(saved()["people"][0]["menu"], serde_json::json!(menu));
    drop(agent);
    drop(hub);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let hub = start_hub_on(&state, MENU, fake.clone(), true).await;
    let _agent = Agent::connect(&hub, true).await;
    hub.until("the private topic's status again", |fake| {
        fake.layout(owner()).last().is_some_and(|m| m == "STATUS")
    })
    .await;
    assert_eq!(fake.menu(owner()).unwrap().id, menu, "no second menu");
    hub.menu_press("menu:d").await;
    let shown = fake.menu(owner()).unwrap();
    assert!(shown.buttons.contains(&"✅ Кратко".to_owned()), "{shown:?}");
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some("готово".into()),
    })
    .await;
    hub.until("the quiet answer", |fake| {
        fake.shown(owner())
            .iter()
            .any(|m| m.text == "готово" && !m.loud)
    })
    .await;
    assert_eq!(saved()["people"][0]["settings"], want);
}

/// A shared slot: «Ничего» quiets the owner's private topic only; the
/// group's copy of the answer rings as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_the_owners_sound_setting_quiets_the_private_topic_only() {
    let mode = Mode::Private {
        share_new: true,
        menu: true,
        mentions: false,
    };
    let (hub, _agent) = menu_hub("menu-sound", mode, false).await;
    hub.menu_press("menu:n").await;
    hub.menu_press("menu:sn:o").await;
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some("готово".into()),
    })
    .await;
    let answer = |fake: &Fake, chat: Chat| {
        fake.shown(chat)
            .into_iter()
            .find(|m| m.text == "готово")
            .map(|m| m.loud)
    };
    hub.until("the answer in both", |fake| {
        answer(fake, owner()).is_some() && answer(fake, GROUP).is_some()
    })
    .await;
    assert_eq!(answer(&hub.fake, owner()), Some(false));
    assert_eq!(answer(&hub.fake, GROUP), Some(true));
}

/// The session buttons of the menu: 👥 shares the slot to the group, 🙈
/// takes it out after a second press, ⏹ interrupts after a second press.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_menu_session_buttons_share_unshare_and_stop() {
    let (hub, mut agent) = menu_hub("menu-buttons", MENU, true).await;
    hub.menu_press("menu:sh:0:0").await;
    let line = share_line(NAME);
    hub.until("the group topic with the share line", |fake| {
        fake.layout(GROUP).first() == Some(&line)
    })
    .await;
    // The share's own edit shows 🙈 already.
    assert!(
        hub.fake
            .menu(owner())
            .unwrap()
            .buttons
            .contains(&"🙈 1".to_owned())
    );
    hub.menu_press("menu:us:0:0").await;
    assert_eq!(hub.fake.topics(GROUP), 1, "one press does not unshare");
    hub.menu_press("menu:uc:0:0").await;
    hub.until("the group topic deleted", |fake| fake.topics(GROUP) == 0)
        .await;
    // ⏹: a turn runs.
    hub.hook(HookEvent::UserPromptSubmit { prompt_id: None })
        .await;
    hub.until("the status offers ⏹", |fake| {
        fake.status(owner())
            .is_some_and(|status| status.buttons.iter().any(|b| b.contains('⏹')))
    })
    .await;
    hub.menu_press("menu:st:0:0").await;
    hub.until("asked for a second press", |fake| {
        fake.answers()
            .contains(&Some(status::ANSWER_CONFIRM.to_owned()))
    })
    .await;
    assert!(
        hub.fake
            .menu(owner())
            .unwrap()
            .buttons
            .contains(&"⏹ 1 точно?".to_owned())
    );
    let menu = hub.fake.menu(owner()).unwrap().id;
    hub.press_general(owner(), menu, "menu:sc:0:0");
    loop {
        match agent.next().await {
            Some(HubMsg::ConsoleKey { .. }) => break,
            Some(_) => {}
            None => panic!("no console key"),
        }
    }
}

/// Only the person changes their settings: a menu press in the group, and
/// one from another allowlisted user's private chat about the owner's
/// slot, change nothing and edit nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_only_the_person_changes_their_settings() {
    let (hub, _agent) = menu_hub("menu-person", MENU, true).await;
    let menu = hub.fake.menu(owner()).unwrap().id;
    let reached = async {
        while hub.saved()["people"][0]["menu"] != serde_json::json!(menu) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(WAIT, reached)
        .await
        .expect("the menu is saved");
    let before = hub.saved()["people"].clone();
    hub.press_general(GROUP, 5, "menu:dl:a");
    let other = Chat::Private(PrivateChat::of_user(OWNER + 1));
    hub.press_general(other, 5, "menu:st:0:0");
    hub.until("two answers", |fake| fake.answers().len() >= 2)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hub.fake.answers(),
        [
            Some(menu::ANSWER_PRIVATE_ONLY.to_owned()),
            Some(menu::ANSWER_NOT_YOURS.to_owned()),
        ]
    );
    assert_eq!(hub.fake.edits_of(owner(), menu), 0);
    assert_eq!(hub.saved()["people"], before);
}

// ---------------------------------------------------------------- TASK-076

/// The session's transcript as its agent serves it: the lines a test
/// adds, 10 bytes each.
#[derive(Clone, Default)]
struct Transcript(Arc<Mutex<Vec<StreamLine>>>);

impl Transcript {
    fn push(&self, items: Vec<StreamItem>) {
        let mut lines = self.0.lock().unwrap();
        let end = lines.last().map_or(0, |line| line.end) + 10;
        lines.push(StreamLine { end, items });
    }

    /// The answer to a read from byte `from` (`None`: the end).
    fn chunk(&self, session_id: String, from: Option<u64>) -> AgentMsg {
        let lines = self.0.lock().unwrap();
        let from = from.unwrap_or_else(|| lines.last().map_or(0, |line| line.end));
        let lines: Vec<StreamLine> = lines
            .iter()
            .filter(|line| line.end > from)
            .cloned()
            .collect();
        AgentMsg::TranscriptChunk {
            session_id,
            from,
            to: lines.last().map_or(from, |line| line.end),
            lines,
            missing: false,
            more: false,
            reset: false,
        }
    }
}

/// The turn message of the last turn: the last message that starts with
/// its text.
fn turn_message(fake: &Fake) -> Option<Shown> {
    fake.shown(owner())
        .into_iter()
        .rev()
        .find(|shown| shown.text.starts_with("Смотрю."))
}

const TOOL_LINES: &str = "• Bash: c1 ✓\n• Bash: c2 ✓\n• Bash: c3 ✓\n• Bash: c4 ✓\n• Bash: c5 ✓";

/// One turn `n` through the transcript, one line at a time, each shown
/// before the next goes (so each line is one read and one write): a
/// prompt, text, five calls, 💭, then the answer and the turn's end. The
/// turn message, and the writes that grew it.
async fn stream_turn(hub: &Hub, transcript: &Transcript, n: u32) -> (Shown, usize) {
    let from = hub.fake.ops().len();
    let prompt = format!("> go {n}");
    transcript.push(vec![StreamItem::Prompt {
        text: format!("go {n}"),
    }]);
    hub.until("the prompt", |fake| fake.layout(owner()).contains(&prompt))
        .await;
    transcript.push(vec![StreamItem::Note {
        text: "Смотрю.".into(),
    }]);
    hub.until("the turn message", |fake| {
        turn_message(fake).is_some_and(|shown| shown.text == "Смотрю.")
    })
    .await;
    for call in 1..=5 {
        let id = format!("c{call}");
        transcript.push(vec![
            StreamItem::Call {
                id: id.clone(),
                line: format!("• Bash: {id}"),
            },
            StreamItem::Result { id, error: None },
        ]);
        let line = format!("• Bash: c{call} ✓");
        hub.until(&line, |fake| {
            turn_message(fake).is_some_and(|shown| shown.text.ends_with(&line))
        })
        .await;
    }
    transcript.push(vec![StreamItem::Thinking {
        text: "Готовлю ответ.".into(),
    }]);
    hub.until("the thinking", |fake| {
        turn_message(fake).is_some_and(|shown| shown.text.ends_with("💭 Готовлю ответ."))
    })
    .await;
    let answer = format!("готово {n}");
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some(answer.clone()),
    })
    .await;
    transcript.push(vec![StreamItem::TurnEnd]);
    hub.until("the answer, then the status", |fake| {
        let layout = fake.layout(owner());
        layout.ends_with(&[answer.clone(), "STATUS".to_owned()])
    })
    .await;
    let turn = turn_message(&hub.fake).expect("the turn message");
    assert_eq!(
        turn.text,
        format!("Смотрю.\n{TOOL_LINES}\n💭 Готовлю ответ.")
    );
    let writes = hub.fake.ops()[from..]
        .iter()
        .filter(|op| {
            matches!(op, Op::Stream { chat, into: Some(id), merge: true, .. }
                if *chat == owner() && *id == turn.id)
        })
        .count();
    (turn, writes)
}

/// The compact turn view (TASK-076): the turn message carries its tool
/// lines and 💭 in one collapsed quote, the text outside it, no buttons,
/// and it grows by as many writes as in the full view; the answer is a
/// message of its own. After «Ход: полный» the next turn has no quote.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_compact_turn_quotes_tool_lines() {
    let hub = start_hub("compact-turn", MENU, Fake::default()).await;
    hub.start_streamed().await;
    let transcript = Transcript::default();
    let _agent = Agent::serve(&hub, transcript.clone()).await;
    hub.until("the status and the pinned menu", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned()) && fake.menu(owner()).is_some()
    })
    .await;
    hub.menu_press("menu:tv:c").await;
    let menu = hub.fake.menu(owner()).unwrap();
    assert!(
        menu.buttons.contains(&"✅ Ход: сжатый".to_owned()),
        "{menu:?}"
    );
    let (compact, compact_writes) = stream_turn(&hub, &transcript, 1).await;
    assert_eq!(
        compact.html.as_deref(),
        Some(
            format!("Смотрю.\n<blockquote expandable>{TOOL_LINES}\n💭 Готовлю ответ.</blockquote>")
                .as_str()
        )
    );
    assert!(compact.buttons.is_empty(), "{compact:?}");
    let answer = hub
        .fake
        .shown(owner())
        .into_iter()
        .find(|shown| shown.text == "готово 1")
        .expect("the answer");
    assert!(answer.id > compact.id, "below the turn message");
    assert!(
        !answer
            .html
            .as_deref()
            .is_some_and(|html| html.contains("blockquote")),
        "{answer:?}"
    );

    hub.menu_press("menu:tv:f").await;
    let (full, full_writes) = stream_turn(&hub, &transcript, 2).await;
    assert_ne!(full.id, compact.id);
    assert_eq!(
        full.html.as_deref(),
        Some(format!("Смотрю.\n{TOOL_LINES}\n💭 Готовлю ответ.").as_str())
    );
    assert!(full.buttons.is_empty());
    // One write per line in both views: the quote adds no edit.
    assert_eq!(compact_writes, 6);
    assert_eq!(full_writes, compact_writes);
    // The first turn's message was left as it was.
    assert_eq!(turn_message_by_id(&hub.fake, compact.id), Some(compact));
    assert!(
        hub.fake
            .answers()
            .iter()
            .all(|answer| answer.as_deref() == Some(menu::ANSWER_SAVED))
    );
}

fn turn_message_by_id(fake: &Fake, id: i64) -> Option<Shown> {
    fake.shown(owner()).into_iter().find(|shown| shown.id == id)
}

/// Telegram refusing the quote: the turn message falls back to its plain
/// text with every line, and the topic goes on (the answer, the status).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_refused_quote_falls_back_to_plain_text() {
    let fake = Fake::default();
    fake.refuse_quote.store(true, Ordering::SeqCst);
    let hub = start_hub("compact-refused", MENU, fake).await;
    hub.start_streamed().await;
    let transcript = Transcript::default();
    let _agent = Agent::serve(&hub, transcript.clone()).await;
    hub.until("the status and the pinned menu", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned()) && fake.menu(owner()).is_some()
    })
    .await;
    hub.menu_press("menu:tv:c").await;
    let (turn, _) = stream_turn(&hub, &transcript, 1).await;
    assert_eq!(turn.html, None, "{turn:?}");
    let refused = hub
        .fake
        .ops()
        .iter()
        .filter(|op| {
            matches!(op, Op::Stream { html: Some(html), .. } if html.contains("<blockquote expandable>"))
        })
        .count();
    assert!(refused > 0, "the quote was tried");
    assert_eq!(
        hub.fake.layout(owner()).last().map(String::as_str),
        Some("STATUS")
    );
}

// ---------------------------------------------------------------- TASK-078

/// An agent in the private chat that reads transcripts (TASK-078), like
/// [`Agent::serve`], and also acks every permission verdict and hands it on:
/// its writer for the test's own frames, the verdicts.
async fn serve_with_verdicts(
    hub: &Hub,
    transcript: Transcript,
) -> (
    Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    mpsc::UnboundedReceiver<(String, Behavior)>,
    JoinHandle<()>,
) {
    let Agent { mut reader, write } = Agent::connect_with(hub, true, false, true, false).await;
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let (verdicts, verdicts_rx) = mpsc::unbounded_channel();
    let writer = write.clone();
    let task = tokio::spawn(async move {
        loop {
            let mut line = Vec::new();
            let read = tokio::time::timeout(
                Duration::from_secs(3600),
                wire::read_line(&mut reader, &mut line),
            )
            .await;
            if !matches!(read, Ok(Ok(()))) {
                return;
            }
            let answer = match wire::decode(&line).unwrap() {
                HubMsg::TranscriptRead {
                    session_id, from, ..
                } => transcript.chunk(session_id, from),
                HubMsg::PermissionVerdict {
                    request_id,
                    behavior,
                    verdict_id: Some(verdict_id),
                } => {
                    let _ = verdicts.send((request_id, behavior));
                    AgentMsg::PermissionAck { verdict_id }
                }
                _ => continue,
            };
            wire::write_msg(&mut *writer.lock().await, &answer)
                .await
                .unwrap();
        }
    });
    (write, verdicts_rx, task)
}

/// The last turn message `chat` shows: the last message that starts with
/// its text.
fn turn_message_in(fake: &Fake, chat: Chat) -> Option<Shown> {
    fake.shown(chat)
        .into_iter()
        .rev()
        .find(|shown| shown.text.starts_with("Смотрю."))
}

/// One turn `n` through the transcript: a prompt, text, two calls, 💭, the
/// answer and the turn's end. It returns once both topics show the answer
/// with their one status message below it.
async fn shared_turn(hub: &Hub, transcript: &Transcript, n: u32) {
    transcript.push(vec![StreamItem::Prompt {
        text: format!("go {n}"),
    }]);
    transcript.push(vec![StreamItem::Note {
        text: "Смотрю.".into(),
    }]);
    for call in 1..=2 {
        let id = format!("c{call}");
        transcript.push(vec![
            StreamItem::Call {
                id: id.clone(),
                line: format!("• Bash: {id}"),
            },
            StreamItem::Result { id, error: None },
        ]);
    }
    transcript.push(vec![StreamItem::Thinking {
        text: "Готовлю ответ.".into(),
    }]);
    let answer = format!("готово {n}");
    hub.hook(HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: Some(answer.clone()),
    })
    .await;
    transcript.push(vec![StreamItem::TurnEnd]);
    hub.until("the answer, then the one status, in both topics", |fake| {
        [owner(), GROUP].into_iter().all(|chat| {
            let layout = fake.layout(chat);
            layout.ends_with(&[answer.clone(), "STATUS".to_owned()])
                && layout.iter().filter(|text| *text == "STATUS").count() == 1
        })
    })
    .await;
}

/// TASK-078: a shared slot shows each view by its own settings: the owner's
/// private topic «Кратко», the group «Всё». Prompts, answers and the status
/// show in both, a press on the group's twin decides the prompt; after
/// «👥 Группа → Кратко» and «👤 Личка → Всё» the next turn is the other way
/// round, and the group's setting is saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_shared_slot_shows_brief_in_private_and_everything_in_the_group() {
    let hub = start_hub(
        "per-view",
        Mode::Private {
            share_new: true,
            menu: true,
            mentions: false,
        },
        Fake::default(),
    )
    .await;
    hub.start_streamed().await;
    let transcript = Transcript::default();
    let (writer, mut verdicts, _agent) = serve_with_verdicts(&hub, transcript.clone()).await;
    hub.until("the status in both topics and the pinned menu", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned())
            && fake.layout(GROUP).contains(&"STATUS".to_owned())
            && fake.menu(owner()).is_some()
    })
    .await;
    hub.menu_press("menu:dl:b").await;

    shared_turn(&hub, &transcript, 1).await;
    let full = "Смотрю.\n• Bash: c1 ✓\n• Bash: c2 ✓\n💭 Готовлю ответ.";
    hub.until("the group's turn message with every line", |fake| {
        turn_message_in(fake, GROUP).is_some_and(|shown| shown.text == full)
    })
    .await;
    let private = hub.fake.layout(owner());
    assert!(
        private.iter().all(|text| !text.contains("• Bash")),
        "{private:#?}"
    );
    assert_eq!(
        turn_message_in(&hub.fake, owner()).map(|shown| shown.text),
        Some("Смотрю.\n💭 Готовлю ответ.".to_owned())
    );
    let group = hub.fake.layout(GROUP);
    assert!(group.contains(&"> go 1".to_owned()), "{group:#?}");
    let answer = hub
        .fake
        .shown(GROUP)
        .into_iter()
        .find(|shown| shown.text == "готово 1")
        .expect("the answer in the group");
    assert!(answer.loud, "{answer:?}");
    assert_eq!(
        hub.fake.layout(GROUP).last().map(String::as_str),
        Some("STATUS")
    );

    // A prompt shows in both; a press on the group's twin decides it.
    wire::write_msg(&mut *writer.lock().await, &permission("abcde"))
        .await
        .unwrap();
    hub.until("the prompt in both topics", |fake| {
        asks_in(fake, owner(), "abcde") && asks_in(fake, GROUP, "abcde")
    })
    .await;
    let twin = prompt_in(&hub.fake, GROUP, "abcde").unwrap().id;
    hub.press(GROUP, twin, "allow:abcde");
    let verdict = tokio::time::timeout(WAIT, verdicts.recv())
        .await
        .expect("a verdict");
    assert_eq!(verdict, Some(("abcde".to_owned(), Behavior::Allow)));
    hub.until("both prompts show the decision", |fake| {
        allowed_in(fake, owner(), "abcde") && allowed_in(fake, GROUP, "abcde")
    })
    .await;

    // The other way round.
    hub.menu_press("menu:dg").await;
    let menu = hub.fake.menu(owner()).unwrap();
    assert!(
        menu.text.starts_with("Что показывать в темах группы"),
        "{menu:?}"
    );
    assert!(menu.buttons.contains(&"· 👁 Показ".to_owned()), "{menu:?}");
    assert!(
        menu.buttons.contains(&"✅ 👥 Группа".to_owned()),
        "{menu:?}"
    );
    hub.menu_press("menu:gdl:b").await;
    hub.menu_press("menu:dl:f").await;
    shared_turn(&hub, &transcript, 2).await;
    hub.until("the private turn message with every line", |fake| {
        turn_message_in(fake, owner()).is_some_and(|shown| shown.text == full)
    })
    .await;
    assert_eq!(
        turn_message_in(&hub.fake, GROUP).map(|shown| shown.text),
        Some("Смотрю.\n💭 Готовлю ответ.".to_owned())
    );
    let group = hub.fake.layout(GROUP);
    let second = group
        .iter()
        .position(|text| text == "> go 2")
        .expect("the second prompt");
    assert!(
        group[second..].iter().all(|text| !text.contains("• Bash")),
        "{group:#?}"
    );
    assert_eq!(
        hub.fake.layout(GROUP).last().map(String::as_str),
        Some("STATUS")
    );
    let saved = async {
        while hub.saved()["people"][0]["settings"]["group"]["detail"] != "brief" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(WAIT, saved)
        .await
        .expect("the group's setting saved");
    assert_eq!(hub.saved()["people"][0]["settings"]["detail"], "full");
}

/// The next compression the agent is asked for; an inbound before it is a
/// failure.
async fn next_compress(agent: &mut Agent) -> (u64, String, u32) {
    loop {
        match agent.next().await {
            Some(HubMsg::SessionRead {
                read_id,
                ask: SessionAsk::Compress { text, limit },
                ..
            }) => return (read_id, text, limit),
            Some(HubMsg::Inbound { content, .. }) => panic!("inbound first: {content}"),
            Some(_) => {}
            None => panic!("no compression asked"),
        }
    }
}

/// TASK-077: the group topic of a shared slot hands the session only what
/// addresses the agent (`@name` in any case, a reply to the bot), with the
/// group's messages since the last one and their authors; the group gets
/// one hint and the private chat no echo of what was kept. A history over
/// the owner's limit (2000 from the menu) is compressed by the agent, or
/// cut when it cannot; 📣 in the menu hands every message at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_shared_group_topic_answers_mentions_with_the_history() {
    let hub = start_hub("mentions", MENTIONS, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect_with(&hub, true, false, false, true).await;
    hub.until("the status and the pinned menu", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned()) && fake.menu(owner()).is_some()
    })
    .await;
    hub.menu_press("menu:sh:0:0").await;
    let line = share_line(NAME);
    hub.until("the group topic with the share line", |fake| {
        fake.layout(GROUP).first() == Some(&line)
    })
    .await;
    assert!(
        hub.fake
            .menu(owner())
            .unwrap()
            .buttons
            .contains(&"📣 1".to_owned())
    );
    let hint = mention::mention_hint(BOT);
    hub.say_group("Анна", "где логи?", None);
    hub.say_group("Иван", "в /var/log", None);
    hub.say_group("Анна", "ок", Some(BOT_ID + 1));
    hub.until("the hint", |fake| fake.layout(GROUP).contains(&hint))
        .await;
    let mention = hub.say_group("Иван", "@CCTG_test_bot сделай", None);
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        content,
        "(история темы группы с прошлого обращения к вам: 3 сообщения)\n\
         Анна: где логи?\n\n---\n\nИван: в /var/log\n\n---\n\nАнна: ок\n(конец истории)\n\n\
         (обращение к вам из группы, где открыта эта сессия)\nИван: сделай"
    );
    assert_eq!(meta["message_id"], mention.to_string());
    assert_eq!(meta["mention"], "true", "TASK-080");
    assert!(!meta.contains_key("history"), "{meta:?}");
    hub.until("the mention echoed in private and 👀", |fake| {
        fake.layout(owner())
            .iter()
            .any(|text| text.ends_with("Иван: @CCTG_test_bot сделай"))
            && fake.ops().iter().any(|op| {
                matches!(op, Op::React { chat: GROUP, message_id, .. } if *message_id == mention)
            })
    })
    .await;
    let group = hub.fake.layout(GROUP);
    assert_eq!(group.iter().filter(|text| **text == hint).count(), 1);
    assert!(
        !hub.fake
            .layout(owner())
            .iter()
            .any(|text| text.contains("где логи") || text.contains("/var/log")),
        "no echo of a kept message"
    );
    // A reply to the bot's message is a mention.
    hub.say_group("Анна", "ещё", None);
    hub.say_group("Иван", "и это", Some(BOT_ID));
    let (content, _) = agent.inbound().await;
    assert_eq!(
        content,
        "(история темы группы с прошлого обращения к вам: 1 сообщение)\n\
         Анна: ещё\n(конец истории)\n\n\
         (обращение к вам из группы, где открыта эта сессия)\nИван: и это"
    );
    // The owner's limit 2000: a longer history is compressed by the agent.
    hub.menu_press("menu:ghl:s").await;
    let (older, newer) = ("я".repeat(1200), "ю".repeat(1200));
    hub.say_group("Анна", &older, None);
    hub.say_group("Иван", &newer, None);
    hub.say_group("Анна", "@cctg_test_bot итог", None);
    let (read_id, text, limit) = next_compress(&mut agent).await;
    assert_eq!(limit, 2000);
    assert_eq!(text, format!("Анна: {older}\n\n---\n\nИван: {newer}"));
    agent
        .send(AgentMsg::SessionAnswer {
            read_id,
            answer: SessionAnswer::Text {
                text: "Анна и Иван прислали по букве".into(),
                more: false,
            },
        })
        .await;
    let (content, _) = agent.inbound().await;
    assert_eq!(
        content,
        "(история темы группы с прошлого обращения к вам: 2 сообщения, сжато)\n\
         Анна и Иван прислали по букве\n(конец истории)\n\n\
         (обращение к вам из группы, где открыта эта сессия)\nАнна: итог"
    );
    // An agent that cannot compress: the newest part only.
    hub.say_group("Анна", &older, None);
    hub.say_group("Иван", &newer, None);
    hub.say_group("Анна", "@cctg_test_bot ещё итог", None);
    let (read_id, _, _) = next_compress(&mut agent).await;
    agent
        .send(AgentMsg::SessionAnswer {
            read_id,
            answer: SessionAnswer::Unsupported,
        })
        .await;
    let (content, _) = agent.inbound().await;
    assert_eq!(
        content,
        format!(
            "(история темы группы с прошлого обращения к вам: 2 сообщения, начало обрезано)\n\
             Иван: {newer}\n(конец истории)\n\n\
             (обращение к вам из группы, где открыта эта сессия)\nАнна: ещё итог"
        )
    );
    // 📣: every message goes at once.
    hub.menu_press("menu:ma:0:0").await;
    hub.until("the mode line in the group", |fake| {
        fake.layout(GROUP)
            .contains(&mention::MODE_ALL_TEXT.to_owned())
    })
    .await;
    hub.say_group("Анна", "без обращения", None);
    let (content, meta) = agent.inbound().await;
    assert_eq!(content, "Анна: без обращения");
    assert!(!meta.contains_key("mention"), "{meta:?}");
    assert!(
        hub.fake
            .menu(owner())
            .unwrap()
            .buttons
            .contains(&"💬 1".to_owned())
    );
    assert!(!texts(&hub.fake.ops()).contains(&OWNER.to_string()));
}

// ---------------------------------------------------------------- TASK-069

/// The second group of these tests.
const B_ID: i64 = -1_002_222;
const B_TITLE: &str = "Команда";

fn group_b() -> Chat {
    Chat::Group(GroupChat::of(B_ID))
}

impl Hub {
    /// The bot's membership in group `id` as `my_chat_member` hands it over.
    fn member(&self, id: i64, status: &str, by_allowed: bool) {
        self.control
            .send(Control::Member(MemberUpdate {
                chat: GroupChat::of(id),
                supergroup: true,
                title: Some(B_TITLE.into()),
                is_forum: true,
                member: ChatMember {
                    status: status.into(),
                    can_manage_topics: true,
                    can_delete_messages: true,
                    ..ChatMember::default()
                },
                by_allowed,
                by: by_allowed.then(|| PrivateChat::of_user(OWNER)),
            }))
            .unwrap();
    }

    /// The owner adds the bot to group B as an administrator.
    async fn join_b(&self) {
        self.member(B_ID, "administrator", true);
        self.until("group B is told it is connected", |fake| {
            fake.general(group_b()) == [groups::GROUP_READY_NOTICE]
        })
        .await;
    }

    /// Team member `name`'s message in the topic of group `chat`.
    fn say_in_group(&self, chat: Chat, name: &str, text: &str) -> i64 {
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
                from_name: Some(name.into()),
                from_username: false,
                author: Some(name.into()),
                display_name: Some(name.into()),
                reply_from: None,
            }))
            .unwrap();
        message_id
    }
}

/// The group picker in the owner's topic, once it shows `label`.
async fn picker(hub: &Hub, label: &str) -> Shown {
    hub.until("the group picker", |fake| {
        fake.shown(owner())
            .iter()
            .any(|shown| shown.buttons.iter().any(|button| button == label))
    })
    .await;
    hub.fake
        .shown(owner())
        .into_iter()
        .rev()
        .find(|shown| shown.buttons.iter().any(|button| button == label))
        .unwrap()
}

/// The data of a picker button of group `id`.
fn pick(id: i64, action: &str) -> String {
    format!("grp:0:{id}:{action}")
}

/// A live session in the private chat, group B joined, the picker open.
async fn picker_hub(name: &str, mode: Mode) -> (Hub, Agent, Shown) {
    let hub = start_hub(name, mode, Fake::default()).await;
    hub.start().await;
    let agent = Agent::connect_with(&hub, true, false, false, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()).contains(&"STATUS".to_owned())
    })
    .await;
    hub.join_b().await;
    hub.until("the status offers the groups", |fake| {
        fake.shown(owner())
            .iter()
            .any(|shown| shown.buttons.iter().any(|b| b == status::GROUPS_BUTTON))
    })
    .await;
    let status = hub
        .fake
        .shown(owner())
        .into_iter()
        .rev()
        .find(|shown| shown.buttons.iter().any(|b| b == status::GROUPS_BUTTON))
        .unwrap();
    hub.press(owner(), status.id, "status:groups");
    hub.until("the press is answered", |fake| {
        fake.answers().contains(&Some(ANSWER_PICKER.to_owned()))
    })
    .await;
    let shown = picker(&hub, &format!("👥 {B_TITLE}")).await;
    assert!(
        shown.buttons.contains(&"👥 основная группа".to_owned()),
        "{shown:?}"
    );
    (hub, agent, shown)
}

/// A second group joins while the hub runs: no restart, no `.env`; the
/// registry keeps it and the poll lets it through.
#[tokio::test]
async fn e2e_groups_a_second_group_connects_without_a_restart() {
    let hub = start_hub("groups-connect", PRIVATE, Fake::default()).await;
    hub.start().await;
    let _agent = Agent::connect(&hub, true).await;
    assert!(!hub.groups.contains(B_ID));
    hub.join_b().await;
    assert!(hub.groups.contains(B_ID) && hub.groups.contains(GROUP_ID));
    hub.until("both groups in registry.json", |_| {
        hub.saved()["groups"]
            .as_array()
            .is_some_and(|groups| groups.len() == 2)
    })
    .await;
    let saved = hub.saved();
    assert_eq!(saved["version"], 3);
    assert_eq!(saved["groups"][1]["chat"], B_ID);
    assert_eq!(saved["groups"][1]["title"], B_TITLE);
    assert_eq!(saved["groups"][1]["ready"], true);
    hub.until("the status offers the groups", |fake| {
        status_has(fake, owner(), status::GROUPS_BUTTON)
    })
    .await;
    // Someone outside the allowlist adds the bot to a third group: it
    // leaves, the group stays unknown and is written nothing.
    let third = Chat::Group(GroupChat::of(-1_003_333));
    hub.member(-1_003_333, "administrator", false);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!hub.groups.contains(-1_003_333));
    assert!(hub.fake.ops().iter().all(|op| op.chat() != Some(third)));
}

/// The owner shares into the chosen group through the picker and takes it
/// out again: only that group gets a topic, a message there reaches the
/// session, and the unshare deletes its topic alone.
#[tokio::test]
async fn e2e_groups_share_into_the_chosen_group_and_unshare() {
    let (hub, mut agent, shown) = picker_hub("groups-share", PRIVATE).await;
    hub.press(owner(), shown.id, &pick(B_ID, "s"));
    let line = share_line(NAME);
    hub.until("group B's topic: the line, then the status", |fake| {
        fake.layout(group_b()) == [line.as_str(), "STATUS"]
    })
    .await;
    assert_eq!(hub.fake.topics(GROUP), 0, "no topic in the default group");
    hub.until("the picker shows B shared", |fake| {
        fake.shown(owner())
            .iter()
            .any(|m| m.id == shown.id && m.buttons.contains(&format!("🙈 {B_TITLE}")))
    })
    .await;
    hub.say_in_group(group_b(), "Иван", "из группы Б");
    let (content, meta) = agent.inbound().await;
    assert_eq!(
        (content.as_str(), meta["place"].as_str()),
        ("Иван: из группы Б", "group")
    );
    hub.press(owner(), shown.id, &pick(B_ID, "u"));
    hub.until("the unshare asks again", |fake| {
        fake.shown(owner())
            .iter()
            .any(|m| m.id == shown.id && m.buttons.contains(&format!("🙈 {B_TITLE} — точно?")))
    })
    .await;
    hub.press(owner(), shown.id, &pick(B_ID, "c"));
    hub.until("group B's topic deleted", |fake| {
        fake.topics(group_b()) == 0
    })
    .await;
    hub.until("the slot in the private chat alone", |_| {
        hub.saved()["slots"][0]["views"]
            .as_array()
            .is_some_and(|views| views.len() == 1)
    })
    .await;
    assert_eq!(hub.fake.topics(GROUP), 0);
}

/// Each group has its own budget: while the default group is on a 429
/// pause, group B and the private chat get every reply at once; the default
/// group is told of its gap once it caught up.
#[tokio::test]
async fn e2e_groups_each_group_has_its_own_budget() {
    let (hub, mut agent, shown) = picker_hub("groups-budget", PRIVATE).await;
    hub.press(owner(), shown.id, &pick(GROUP_ID, "s"));
    hub.press(owner(), shown.id, &pick(B_ID, "s"));
    let line = share_line(NAME);
    hub.until("both group topics", |fake| {
        fake.layout(GROUP) == [line.as_str(), "STATUS"]
            && fake.layout(group_b()) == [line.as_str(), "STATUS"]
    })
    .await;
    *hub.fake.flood.lock().unwrap() = Some(GROUP);
    let count = MAX_TWIN_POSTS + 20;
    for n in 0..count {
        agent
            .send(AgentMsg::Reply {
                text: format!("r{n}"),
            })
            .await;
    }
    let last = format!("r{}", count - 1);
    hub.until("B and the private chat have every reply", |fake| {
        fake.layout(group_b()).contains(&last) && fake.layout(owner()).contains(&last)
    })
    .await;
    assert!(
        !hub.fake.layout(GROUP).contains(&last),
        "the default group waits"
    );
    *hub.fake.flood.lock().unwrap() = None;
    hub.until("the default group is told of its gap", |fake| {
        fake.layout(GROUP).contains(&MIRROR_GAP_NOTICE.to_owned())
    })
    .await;
    assert!(
        !hub.fake
            .layout(group_b())
            .contains(&MIRROR_GAP_NOTICE.to_owned()),
        "B missed nothing"
    );
    let replies = hub
        .fake
        .layout(group_b())
        .iter()
        .filter(|text| text.starts_with('r'))
        .count();
    assert_eq!(replies, count);
}

/// A group the bot was removed from gets nothing; back as an
/// administrator, it gets the session again.
#[tokio::test]
async fn e2e_groups_a_removed_group_gets_nothing_and_comes_back() {
    let (hub, mut agent, shown) = picker_hub("groups-removed", PRIVATE).await;
    hub.press(owner(), shown.id, &pick(B_ID, "s"));
    let line = share_line(NAME);
    hub.until("group B's topic", |fake| {
        fake.layout(group_b()) == [line.as_str(), "STATUS"]
    })
    .await;
    hub.member(B_ID, "kicked", false);
    hub.until("B left in registry.json", |_| {
        hub.saved()["groups"][1]["left"] == true
    })
    .await;
    let from = hub.fake.ops().len();
    agent
        .send(AgentMsg::Reply {
            text: "без группы".into(),
        })
        .await;
    hub.until("the reply in private", |fake| {
        fake.layout(owner()).contains(&"без группы".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let into_b: Vec<Op> = hub.fake.ops()[from..]
        .iter()
        .filter(|op| op.chat() == Some(group_b()))
        .cloned()
        .collect();
    assert!(
        into_b.is_empty(),
        "nothing goes to a group the bot left: {into_b:#?}"
    );
    hub.member(B_ID, "administrator", true);
    hub.until("B is told it is back", |fake| {
        fake.general(group_b()).len() == 2
    })
    .await;
    agent
        .send(AgentMsg::Reply {
            text: "снова".into(),
        })
        .await;
    hub.until("B gets the session again", |fake| {
        fake.layout(group_b()).contains(&"снова".to_owned())
    })
    .await;
    assert!(
        !hub.fake
            .layout(group_b())
            .contains(&"без группы".to_owned())
    );
}

/// `/connect@bot` in a group without topics, where the bot may not manage
/// topics: the answer lists both, where the command was written.
#[tokio::test]
async fn e2e_groups_connect_tells_what_is_missing() {
    let fake = Fake::default();
    fake.no_rights.store(true, Ordering::SeqCst);
    let hub = start_hub("groups-connect-missing", MENTIONS, fake).await;
    let chat = Chat::Group(GroupChat::of(-1_003_333));
    hub.control
        .send(Control::Connect(ConnectInput {
            chat: GroupChat::of(-1_003_333),
            supergroup: true,
            title: Some("Без тем".into()),
            is_forum: false,
            thread_id: None,
            target: Some(BOT.to_uppercase()),
            sender: PrivateChat::of_user(OWNER),
        }))
        .unwrap();
    hub.until("the answer in the group", |fake| {
        !fake.general(chat).is_empty()
    })
    .await;
    let told = hub.fake.general(chat);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("включите темы"), "{told:?}");
    assert!(told[0].contains("\"Управление темами\""), "{told:?}");
    assert!(told[0].contains(&format!("/connect@{BOT}")), "{told:?}");
    assert!(hub.groups.contains(-1_003_333), "known, not ready");
}

/// Mention mode per group: each group topic keeps its own messages; a
/// mention in B takes B's history only.
#[tokio::test]
async fn e2e_groups_mentions_per_group() {
    let (hub, mut agent, shown) = picker_hub("groups-mentions", MENTIONS).await;
    hub.press(owner(), shown.id, &pick(GROUP_ID, "s"));
    hub.press(owner(), shown.id, &pick(B_ID, "s"));
    let line = share_line(NAME);
    hub.until("both group topics", |fake| {
        fake.layout(GROUP).first() == Some(&line) && fake.layout(group_b()).first() == Some(&line)
    })
    .await;
    hub.say_in_group(GROUP, "Анна", "в группе А");
    hub.say_in_group(group_b(), "Иван", "в группе Б");
    let hint = mention::mention_hint(BOT);
    hub.until("a hint in each topic", |fake| {
        fake.layout(GROUP).contains(&hint) && fake.layout(group_b()).contains(&hint)
    })
    .await;
    assert!(
        agent
            .next_within(Duration::from_millis(200))
            .await
            .is_none_or(|msg| !matches!(msg, HubMsg::Inbound { .. }))
    );
    hub.say_in_group(group_b(), "Иван", "@cctg_test_bot что скажешь?");
    let (content, _) = agent.inbound().await;
    assert!(content.contains("Иван: в группе Б"), "{content}");
    assert!(!content.contains("в группе А"), "{content}");
    assert!(
        content
            .ends_with("(обращение к вам из группы, где открыта эта сессия)\nИван: что скажешь?"),
        "{content}"
    );
}

// ---------------------------------------------------------------- TASK-072

/// A private-only slot met a 403 while a prompt was open, and the prompt
/// went to the group's fallback topic (prompt `abcde`); then the owner
/// pressed Start. The hub and its agent.
async fn fallback_prompt_then_start(name: &str) -> (Hub, Agent) {
    let hub = start_hub(name, PRIVATE, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.until("the status in the private chat", |fake| {
        fake.layout(owner()) == ["STATUS"]
    })
    .await;
    hub.fake.forbid_private.store(true, Ordering::SeqCst);
    agent.send(permission("abcde")).await;
    hub.until("the prompt in the group", |fake| {
        asks_in(fake, GROUP, "abcde")
    })
    .await;
    hub.fake.forbid_private.store(false, Ordering::SeqCst);
    hub.say_general(owner(), "/start");
    (hub, agent)
}

/// Review 2 of TASK-063, finding 1: after Start the slot leaves the group
/// at once, and the prompt that waited in the group topic is not lost: it
/// comes again into the private topic with its buttons, the group copy
/// loses them. A message written into the old group topic reaches no
/// session and gets the fallback-end answer; the press in private decides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_prompt_of_an_ended_fallback_topic_goes_into_the_private_chat() {
    let (hub, mut agent) = fallback_prompt_then_start("fallback-open-prompt").await;
    let notice = FALLBACK_END_NOTICE.to_owned();
    hub.until("the group told, the prompt in private", |fake| {
        fake.layout(GROUP).contains(&notice)
            && asks_in(fake, owner(), "abcde")
            && prompt_in(fake, GROUP, "abcde")
                .is_some_and(|copy| copy.buttons.is_empty() && copy.text.ends_with(MOVED_MARK))
    })
    .await;
    hub.say(GROUP, "в старую тему");
    hub.until("the old topic answered", |fake| {
        fake.layout(GROUP)
            .iter()
            .filter(|text| **text == notice)
            .count()
            == 2
    })
    .await;
    assert!(
        !matches!(
            agent.next_within(Duration::from_millis(300)).await,
            Some(HubMsg::Inbound { .. })
        ),
        "the old group topic reaches no session"
    );
    allow_in(&hub, &mut agent, owner(), "abcde").await;
    hub.until("decided in private", |fake| {
        allowed_in(fake, owner(), "abcde")
    })
    .await;
}

/// Code review of TASK-072 (R1): what the owner writes in the private topic
/// after Start, and the session's answers there, never reach the group
/// topic the prompt waited in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_after_start_private_text_stays_out_of_the_old_fallback_topic() {
    let (hub, mut agent) = fallback_prompt_then_start("fallback-no-echo").await;
    hub.until("the prompt in private", |fake| {
        asks_in(fake, owner(), "abcde")
    })
    .await;
    hub.say(owner(), "секрет из лички");
    let (content, _) = agent.inbound().await;
    assert_eq!(content, "секрет из лички");
    agent
        .send(AgentMsg::Reply {
            text: "ответ только для лички".into(),
        })
        .await;
    hub.until("the reply in private", |fake| {
        fake.layout(owner())
            .contains(&"ответ только для лички".to_owned())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let group = hub.fake.layout(GROUP);
    assert!(
        !group
            .iter()
            .any(|text| text.contains("секрет из лички") || text.contains("ответ только для лички")),
        "private text in the group: {group:?}"
    );
}

/// Review 2 of TASK-063, finding 7: a shared slot whose owner blocked the
/// bot (403) gets a reply: the group, the slot's view now, shows it once,
/// with the status message below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_after_a_403_the_group_status_goes_below_the_reply() {
    let hub = start_hub("403-status-below", SHARED, Fake::default()).await;
    hub.start().await;
    let mut agent = Agent::connect(&hub, true).await;
    hub.both("a status message in both views", &["STATUS"])
        .await;
    hub.fake.forbid_private.store(true, Ordering::SeqCst);
    agent
        .send(AgentMsg::Reply {
            text: "раз".into()
        })
        .await;
    hub.until("the reply once, the status below it", |fake| {
        fake.layout(GROUP) == ["раз", "STATUS"]
    })
    .await;
    // And it stays so (the review saw ["STATUS", "раз"] for 3 s).
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(hub.fake.layout(GROUP), ["раз", "STATUS"]);
}

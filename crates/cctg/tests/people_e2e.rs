//! The people of the menu end to end (TASK-081): raw `getUpdates` batches
//! through the real update poll ([`updates::poll_until`]) with the same
//! `Allowlist` the real `Slots` actor writes, the real `/join` worker, an
//! agent over a real TCP link to the real `serve_agents`, and a fake Bot API
//! that keeps every chat on its own. The owner adds a person by forwarding
//! their message and by a one-time invite link, the person writes to the
//! session at once (no restart, no `.env`), a greeting refused with 403
//! tells the group nothing, and two presses on the owner's menu close the
//! person's access again, also for what came in the same batch as the
//! removal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::config::Allowlist;
use cctg::hub::devices::Devices;
use cctg::hub::groups::KnownGroups;
use cctg::hub::ingress::{bind, serve_agents};
use cctg::hub::offset::OffsetStore;
use cctg::hub::people;
use cctg::hub::registry::{ICON_ALIVE, RegistryStore};
use cctg::hub::roster;
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{Control, MentionBot, Options, Owners, Slots};
use cctg::hub::updates::{self, Routed, UpdateSource};
use cctg::wire::{
    self, AgentMsg, HookEvent, HookPost, HubMsg, PermissionRequest, Register, Secret,
};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));
const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const CWD: &str = "C:/qa/people";
const SESSION: &str = "0a16e2e0-0000-4000-8000-000000000811";
/// The owner (`CCTG_ALLOWED_USER_IDS`).
const OWNER: i64 = 7_319_402_518;
/// Added by a forward.
const BORIS: i64 = 7_319_402_601;
/// Added while their private chat refuses the bot (never pressed Start).
const SILENT: i64 = 7_319_402_602;
/// Hides their account in forwards: added by an invite link.
const GUEST: i64 = 7_319_402_603;
const BOT_ID: i64 = 8_100_200_300;
const BOT: &str = "cctg_test_bot";
const WAIT: Duration = Duration::from_secs(30);

fn private(user: i64) -> Chat {
    Chat::Private(PrivateChat::of_user(user))
}

// ---------------------------------------------------------------- fake Telegram

/// A message a chat shows.
#[derive(Debug, Clone)]
struct Shown {
    chat: Chat,
    thread: Option<i64>,
    id: i64,
    text: String,
    /// The callback data of its buttons.
    datas: Vec<String>,
}

#[derive(Default)]
struct Telegram {
    shown: Mutex<Vec<Shown>>,
    ops: Mutex<Vec<Op>>,
    next_message: Mutex<HashMap<Chat, i64>>,
    next_topic: Mutex<HashMap<Chat, i64>>,
    /// Private chats whose user never pressed Start: every call is 403.
    forbidden: Mutex<HashSet<Chat>>,
}

fn datas(markup: Option<&Value>) -> Vec<String> {
    markup
        .and_then(|markup| markup["inline_keyboard"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|button| button["callback_data"].as_str().map(str::to_owned))
        .collect()
}

impl Transport for Telegram {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        let Some(chat) = op.chat() else {
            return Ok(Outcome::Done);
        };
        if self.forbidden.lock().unwrap().contains(&chat) {
            return Err(ApiError::Telegram {
                code: 403,
                description: "Forbidden: bot can't initiate conversation with a user".into(),
            });
        }
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
                    datas: datas(reply_markup.as_ref()),
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
                    datas: Vec::new(),
                });
                sent(id)
            }
            Op::Stream {
                text,
                into: Some(id),
                ..
            } => {
                let mut all = self.shown.lock().unwrap();
                if let Some(shown) = all
                    .iter_mut()
                    .find(|shown| shown.chat == chat && shown.id == *id)
                {
                    shown.text.clone_from(text);
                }
                sent(*id)
            }
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
                    shown.datas = datas(reply_markup.as_ref());
                }
                Ok(Outcome::Done)
            }
            Op::Delete { message_id, .. } => {
                self.shown
                    .lock()
                    .unwrap()
                    .retain(|shown| !(shown.chat == chat && shown.id == *message_id));
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Telegram {
    fn all(&self) -> Vec<Shown> {
        self.shown.lock().unwrap().clone()
    }

    /// The messages of the General of `chat`.
    fn general(&self, chat: Chat) -> Vec<Shown> {
        self.all()
            .into_iter()
            .filter(|shown| shown.chat == chat && shown.thread.is_none())
            .collect()
    }

    /// The newest message of the General of `chat` that says `text`.
    fn said(&self, chat: Chat, text: &str) -> Option<Shown> {
        self.general(chat)
            .into_iter()
            .rev()
            .find(|shown| shown.text.contains(text))
    }

    /// The owner's menu: the General message with the people tab.
    fn menu(&self) -> Option<Shown> {
        self.general(private(OWNER))
            .into_iter()
            .rev()
            .find(|shown| shown.datas.iter().any(|data| data == "menu:pp"))
    }

    /// The topic of the slot in `chat`, once made.
    fn topic(&self, chat: Chat) -> Option<i64> {
        self.ops.lock().unwrap().iter().find_map(|op| match op {
            Op::CreateTopic { chat: to, .. } if *to == chat => {
                self.next_topic.lock().unwrap().get(&chat).copied()
            }
            _ => None,
        })
    }

    fn answered(&self, query: &str) -> bool {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .any(|op| matches!(op, Op::AnswerCallback { query_id, .. } if query_id == query))
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

fn user(id: i64, first_name: &str, username: &str) -> Value {
    json!({ "id": id, "is_bot": false, "first_name": first_name, "username": username })
}

/// A message of `from` in the General of `from`'s private chat.
fn in_private(message_id: i64, from: &Value, extra: Value) -> Value {
    let mut message = json!({
        "message_id": message_id, "date": 1, "from": from,
        "chat": { "id": from["id"], "type": "private", "first_name": from["first_name"] },
    });
    if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    json!({ "message": message })
}

/// A message of `from` in group topic `thread`.
fn in_group(message_id: i64, thread: i64, from: &Value, text: &str) -> Value {
    json!({ "message": {
        "message_id": message_id, "date": 1, "from": from, "text": text,
        "message_thread_id": thread, "is_topic_message": true,
        "chat": { "id": GROUP_ID, "type": "supergroup", "is_forum": true },
    }})
}

/// A press of `data` by `from` on message `message_id` of `chat`, in topic
/// `thread` or the General.
fn press(
    query: &str,
    from: &Value,
    chat: Chat,
    thread: Option<i64>,
    message_id: i64,
    data: &str,
) -> Value {
    let chat_json = match chat {
        Chat::Group(_) => json!({ "id": GROUP_ID, "type": "supergroup", "is_forum": true }),
        Chat::Private(_) => json!({ "id": from["id"], "type": "private" }),
    };
    let mut message = json!({ "message_id": message_id, "date": 1, "chat": chat_json });
    if let Some(thread) = thread {
        message["message_thread_id"] = json!(thread);
        message["is_topic_message"] = json!(true);
    }
    json!({ "callback_query": {
        "id": query, "from": from, "chat_instance": "c", "data": data, "message": message,
    }})
}

// ---------------------------------------------------------------- hub

struct Hub {
    telegram: Arc<Telegram>,
    batches: mpsc::UnboundedSender<Vec<Value>>,
    next_update: Mutex<i64>,
    allowlist: Allowlist,
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

/// Hands the poll's routes on as the hub does: commands of `/join` and
/// `/devices` to the device list, the rest to the slot actor.
fn route(
    control: mpsc::UnboundedSender<Control>,
    roster: mpsc::UnboundedSender<roster::Input>,
) -> impl FnMut(Routed) {
    move |routed| {
        let _ = match routed {
            Routed::Input(input) if input.chat.is_private() && roster::is_command(&input) => {
                let _ = roster.send(roster::Input::Command(input));
                return;
            }
            Routed::Callback(input) if roster::is_callback(&input) => {
                let _ = roster.send(roster::Input::Press(input));
                return;
            }
            Routed::Input(input) => control.send(Control::Message(input)),
            Routed::Callback(input) => control.send(Control::Callback(input)),
            Routed::Forward(input) => control.send(Control::Forward(input)),
            Routed::Invite(input) => control.send(Control::Invite(input)),
            Routed::Member(update) => control.send(Control::Member(update)),
            Routed::Connect(input) => control.send(Control::Connect(input)),
            Routed::Service(_) | Routed::Ignored(_) => return,
        };
    }
}

async fn start_hub() -> Hub {
    let state = std::env::temp_dir().join(format!("cctg-people-e2e-{}", std::process::id()));
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
    let devices = Devices::open(&state, None).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        status_every: Some(Duration::from_millis(50)),
        owners: Some(Owners {
            first: PrivateChat::of_user(OWNER),
            devices: None,
            share_new: true,
        }),
        menu: true,
        mentions: Some(MentionBot {
            id: BOT_ID,
            username: BOT.into(),
        }),
        groups: groups.clone(),
        allowlist: allowlist.clone(),
        devices: Some(devices.clone()),
        ..Options::default()
    };
    let slots = Slots::new(registry, store, outbox.clone(), options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let (roster_tx, roster_rx) = mpsc::unbounded_channel();
    let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let agent_addr = listener.local_addr().unwrap();
    let join = roster::JoinInfo {
        release: None,
        public: None,
        agent_listen: agent_addr,
        hook_listen: agent_addr,
        pin: None,
    };
    let (batches, batches_rx) = mpsc::unbounded_channel();
    let offsets = OffsetStore::open(&state).unwrap();
    let poll = {
        let (groups, allowlist) = (groups.clone(), allowlist.clone());
        let source = Batches(tokio::sync::Mutex::new(batches_rx));
        let route = route(control.clone(), roster_tx);
        tokio::spawn(async move {
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
        })
    };
    let tasks = vec![
        tokio::spawn(serve_agents(
            listener,
            Secret::parse(SECRET).unwrap(),
            agents,
        )),
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)),
        tokio::spawn(roster::serve(
            roster_rx,
            outbox,
            devices,
            join,
            Some(BOT.into()),
            allowlist.clone(),
        )),
        poll,
    ];
    Hub {
        telegram,
        batches,
        next_update: Mutex::new(0),
        allowlist,
        agent_addr,
        hooks,
        tasks,
        state,
    }
}

impl Hub {
    /// One `getUpdates` batch of `updates` (each without its `update_id`).
    fn batch(&self, updates: Vec<Value>) {
        let mut next = self.next_update.lock().unwrap();
        let batch = updates
            .into_iter()
            .map(|mut update| {
                *next += 1;
                update["update_id"] = json!(*next);
                update
            })
            .collect();
        self.batches.send(batch).unwrap();
    }

    async fn until(&self, what: &str, ready: impl Fn(&Telegram) -> bool) {
        let reached = async {
            while !ready(&self.telegram) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        if tokio::time::timeout(WAIT, reached).await.is_err() {
            panic!("{what}: {:#?}", self.telegram.all());
        }
    }

    async fn start_session(&self) {
        let post = HookPost::new(
            HOST.into(),
            SESSION.into(),
            CWD.into(),
            String::new(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(4242),
                parent_claude_pid: None,
            },
        );
        self.hooks.send(post).await.unwrap();
    }

    /// The owner confirms the one waiting proposal; waits for its edit.
    async fn confirm(&self, query: &str, owner: &Value) {
        let proposal = self
            .telegram
            .said(private(OWNER), "в cctg?")
            .expect("a proposal");
        let yes = proposal
            .datas
            .iter()
            .find(|data| data.starts_with("menu:pa:"))
            .expect("«Добавить»")
            .clone();
        self.batch(vec![press(
            query,
            owner,
            private(OWNER),
            None,
            proposal.id,
            &yes,
        )]);
        let id = proposal.id;
        self.until("the proposal answered", |telegram| {
            telegram
                .general(private(OWNER))
                .iter()
                .any(|shown| shown.id == id && shown.text.starts_with("Добавлен(а)"))
        })
        .await;
    }

    /// The owner presses `data` on their menu; waits until the press is
    /// answered and the menu is `ready`.
    async fn menu_press(
        &self,
        query: &str,
        owner: &Value,
        data: &str,
        ready: impl Fn(&Shown) -> bool,
    ) {
        let menu = self.telegram.menu().expect("the owner's menu");
        self.batch(vec![press(
            query,
            owner,
            private(OWNER),
            None,
            menu.id,
            data,
        )]);
        let id = menu.id;
        self.until(data, |telegram| {
            telegram
                .general(private(OWNER))
                .iter()
                .any(|shown| shown.id == id && ready(shown))
                && telegram.answered(query)
        })
        .await;
    }
}

// ---------------------------------------------------------------- agent

struct Agent {
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

impl Agent {
    async fn connect(hub: &Hub) -> Self {
        let from = hub.telegram.ops.lock().unwrap().len();
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
            verdict_ack: false,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            client: None,
            files: false,
            session_reads: false,
            status_lines: false,
            private_place: true,
            enrolled: None,
            heartbeat: false,
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
        hub.until("the agent bound: the alive icon", |telegram| {
            telegram.ops.lock().unwrap()[from..].iter().any(|op| {
                matches!(op,
                    Op::CreateTopic { icon_custom_emoji_id: Some(icon), .. }
                    | Op::EditTopic { icon_custom_emoji_id: Some(icon), .. } if icon == ICON_ALIVE)
            })
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

    /// The next inbound message within `wait`: its content and meta.
    async fn inbound_within(
        &mut self,
        wait: Duration,
    ) -> Option<(String, BTreeMap<String, String>)> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_within(left).await {
                Some(HubMsg::Inbound { content, meta }) => return Some((content, meta)),
                Some(_) => {}
                None => return None,
            }
        }
    }

    /// Everything the hub sends within `wait`.
    async fn drain(&mut self, wait: Duration) -> Vec<HubMsg> {
        let mut got = Vec::new();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_within(left).await {
                Some(msg) => got.push(msg),
                None => return got,
            }
        }
    }

    async fn send(&mut self, msg: AgentMsg) {
        wire::write_msg(&mut self.write, &msg).await.unwrap();
    }
}

fn verdicts(msgs: &[HubMsg]) -> usize {
    msgs.iter()
        .filter(|msg| matches!(msg, HubMsg::PermissionVerdict { .. }))
        .count()
}

fn inbounds(msgs: &[HubMsg]) -> usize {
    msgs.iter()
        .filter(|msg| matches!(msg, HubMsg::Inbound { .. }))
        .count()
}

// ---------------------------------------------------------------- the test

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_add_and_remove_people_from_the_menu_without_a_restart() {
    let hub = start_hub().await;
    let owner = user(OWNER, "Анна", "anna");
    let boris = user(BORIS, "Борис", "boris");
    let silent = user(SILENT, "Тихон", "tikhon");
    let guest = user(GUEST, "Гость", "guest");
    hub.telegram
        .forbidden
        .lock()
        .unwrap()
        .insert(private(SILENT));

    hub.start_session().await;
    let mut agent = Agent::connect(&hub).await;
    hub.until("the group topic and the owner's menu", |telegram| {
        telegram.topic(GROUP).is_some() && telegram.menu().is_some()
    })
    .await;
    let thread = hub.telegram.topic(GROUP).unwrap();
    let _ = agent.drain(Duration::from_millis(300)).await;

    // 1. A stranger's mention in the group topic reaches no one.
    hub.batch(vec![in_group(1, thread, &boris, &format!("@{BOT} привет"))]);
    assert!(
        agent
            .inbound_within(Duration::from_millis(500))
            .await
            .is_none(),
        "a stranger is dropped by the poll"
    );

    // 2. The owner forwards Boris's message into their General and adds him.
    let forwarded = json!({ "text": "моё сообщение", "forward_origin": {
        "type": "user", "date": 1, "sender_user": boris } });
    hub.batch(vec![in_private(10, &owner, forwarded)]);
    hub.until("the proposal", |telegram| {
        telegram
            .said(private(OWNER), "Добавить «Борис (@boris)» в cctg?")
            .is_some()
    })
    .await;
    hub.confirm("q-add-boris", &owner).await;
    assert!(hub.allowlist.contains(BORIS));
    hub.until("Boris is greeted", |telegram| {
        telegram.said(private(BORIS), "Вам открыт доступ").is_some()
    })
    .await;
    // His next mention reaches the session, signed: no restart, no .env.
    hub.batch(vec![in_group(
        2,
        thread,
        &boris,
        &format!("@{BOT} теперь можно?"),
    )]);
    let (content, meta) = agent
        .inbound_within(WAIT)
        .await
        .expect("Boris reaches the session");
    assert!(content.contains("теперь можно?"), "{content}");
    assert_eq!(meta.get("from_name").map(String::as_str), Some("boris"));
    // Boris opens the bot: his menu, without the people tab.
    hub.batch(vec![in_private(11, &boris, json!({ "text": "/start" }))]);
    hub.until("Boris's menu", |telegram| {
        telegram
            .general(private(BORIS))
            .iter()
            .any(|shown| shown.datas.iter().any(|data| data == "menu:s:0"))
    })
    .await;
    assert!(
        !hub.telegram
            .general(private(BORIS))
            .iter()
            .any(|shown| shown.datas.iter().any(|data| data.starts_with("menu:p"))),
        "a member has no people tab"
    );
    // A member neither joins devices nor opens the people tab.
    hub.batch(vec![in_private(12, &boris, json!({ "text": "/join" }))]);
    hub.until("the /join refusal", |telegram| {
        telegram.said(private(BORIS), roster::OWNERS_ONLY).is_some()
    })
    .await;
    let boris_menu = hub
        .telegram
        .general(private(BORIS))
        .into_iter()
        .find(|shown| shown.datas.iter().any(|data| data == "menu:s:0"))
        .unwrap();
    hub.batch(vec![press(
        "q-boris-pp",
        &boris,
        private(BORIS),
        None,
        boris_menu.id,
        "menu:pp",
    )]);
    hub.until("the refusal of the people tab", |telegram| {
        telegram.ops.lock().unwrap().iter().any(|op| {
            matches!(op, Op::AnswerCallback { query_id, text: Some(text) }
                if query_id == "q-boris-pp" && text == people::ANSWER_OWNER_ONLY)
        })
    })
    .await;

    // 3. Tikhon never pressed Start: his greeting gets 403, the group hears
    // nothing of it.
    let group_before = hub.telegram.general(GROUP).len();
    let forwarded = json!({ "text": "я тут", "forward_origin": {
        "type": "user", "date": 1, "sender_user": silent } });
    hub.batch(vec![in_private(13, &owner, forwarded)]);
    hub.until("the proposal for Tikhon", |telegram| {
        telegram
            .said(private(OWNER), "Добавить «Тихон (@tikhon)» в cctg?")
            .is_some()
    })
    .await;
    hub.confirm("q-add-silent", &owner).await;
    hub.until("the greeting refused", |telegram| {
        telegram
            .ops
            .lock()
            .unwrap()
            .iter()
            .any(|op| matches!(op, Op::Send { chat, .. } if *chat == private(SILENT)))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hub.telegram.general(GROUP).len(),
        group_before,
        "the group heard nothing: {:#?}",
        hub.telegram.general(GROUP)
    );
    assert!(hub.allowlist.contains(SILENT));

    // 4. The owner's menu lists both; no button removes the owner; a forged
    // second press without the first changes nothing.
    hub.menu_press("q-pp", &owner, "menu:pp", |menu| {
        menu.text.contains("1. Борис (@boris)")
    })
    .await;
    let menu = hub.telegram.menu().unwrap();
    assert!(menu.text.contains("2. Тихон (@tikhon)"), "{}", menu.text);
    let removes: Vec<&String> = menu
        .datas
        .iter()
        .filter(|data| data.starts_with("menu:pr:"))
        .collect();
    assert_eq!(removes.len(), 2, "{:?}", menu.datas);
    let boris_key = removes[0].strip_prefix("menu:pr:").unwrap().to_owned();
    hub.batch(vec![press(
        "q-forged",
        &owner,
        private(OWNER),
        None,
        menu.id,
        &format!("menu:prc:{boris_key}"),
    )]);
    hub.until("the forged press answered", |telegram| {
        telegram.answered("q-forged")
    })
    .await;
    assert!(hub.allowlist.contains(BORIS));

    // A prompt of the session, shown in the group topic too.
    agent
        .send(AgentMsg::PermissionRequest(PermissionRequest {
            request_id: "abcde".into(),
            tool_name: "Bash".into(),
            description: "Run a command".into(),
            input_preview: "ls".into(),
        }))
        .await;
    hub.until("the prompt in the group", |telegram| {
        telegram.all().iter().any(|shown| {
            shown.chat == GROUP && shown.datas.iter().any(|data| data == "allow:abcde")
        })
    })
    .await;
    let prompt = hub
        .telegram
        .all()
        .into_iter()
        .find(|shown| shown.chat == GROUP && shown.datas.iter().any(|data| data == "allow:abcde"))
        .unwrap();

    // 5. Two presses remove Boris. His mention and his Allow in the same
    // batch as the second press come too late.
    let armed = format!("menu:prc:{boris_key}");
    hub.menu_press("q-pr", &owner, &format!("menu:pr:{boris_key}"), |menu| {
        menu.datas.contains(&armed)
    })
    .await;
    let menu = hub.telegram.menu().unwrap();
    hub.batch(vec![
        press(
            "q-prc",
            &owner,
            private(OWNER),
            None,
            menu.id,
            &format!("menu:prc:{boris_key}"),
        ),
        in_group(3, thread, &boris, &format!("@{BOT} ещё раз")),
        press(
            "q-boris-allow",
            &boris,
            GROUP,
            Some(thread),
            prompt.id,
            "allow:abcde",
        ),
    ]);
    hub.until("Boris removed", |telegram| {
        telegram.ops.lock().unwrap().iter().any(|op| {
            matches!(op, Op::AnswerCallback { query_id, text: Some(text) }
                if query_id == "q-prc" && text.starts_with("Удалён(а): Борис (@boris)."))
        })
    })
    .await;
    assert!(!hub.allowlist.contains(BORIS));
    hub.until("Boris is told and his menu goes", |telegram| {
        telegram.said(private(BORIS), "закрыл вам доступ").is_some()
            && !telegram
                .general(private(BORIS))
                .iter()
                .any(|shown| shown.id == boris_menu.id)
    })
    .await;
    // Later ones are dropped by the poll.
    hub.batch(vec![
        in_group(4, thread, &boris, &format!("@{BOT} а сейчас?")),
        press(
            "q-boris-allow-2",
            &boris,
            GROUP,
            Some(thread),
            prompt.id,
            "allow:abcde",
        ),
    ]);
    let late = agent.drain(Duration::from_millis(700)).await;
    assert_eq!(inbounds(&late), 0, "{late:?}");
    assert_eq!(verdicts(&late), 0, "{late:?}");
    assert!(!hub.telegram.answered("q-boris-allow"));
    assert!(!hub.telegram.answered("q-boris-allow-2"));
    // The owner's Allow still counts: the prompt was there to press.
    hub.batch(vec![press(
        "q-owner-allow",
        &owner,
        GROUP,
        Some(thread),
        prompt.id,
        "allow:abcde",
    )]);
    let answered = agent.drain(Duration::from_secs(2)).await;
    assert_eq!(verdicts(&answered), 1, "{answered:?}");

    // 6. Guest hides their account: the owner gets the invite link button,
    // the link asks the owner, «Добавить» lets the guest in.
    let hidden = json!({ "text": "это Гость", "forward_origin": {
        "type": "hidden_user", "date": 1, "sender_user_name": "Гость" } });
    hub.batch(vec![in_private(14, &owner, hidden)]);
    hub.until("the hidden answer", |telegram| {
        telegram.said(private(OWNER), people::HIDDEN).is_some()
    })
    .await;
    let hint = hub.telegram.said(private(OWNER), people::HIDDEN).unwrap();
    assert_eq!(hint.datas, ["menu:pi"]);
    hub.batch(vec![press(
        "q-pi",
        &owner,
        private(OWNER),
        None,
        hint.id,
        "menu:pi",
    )]);
    hub.until("the link", |telegram| {
        telegram
            .said(private(OWNER), "ссылка-приглашение на 10 минут")
            .is_some()
    })
    .await;
    let link = hub
        .telegram
        .said(private(OWNER), "ссылка-приглашение на 10 минут")
        .unwrap()
        .text;
    let code = link
        .split(&format!("https://t.me/{BOT}?start=inv_"))
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the link")
        .to_owned();
    hub.batch(vec![in_private(
        1,
        &guest,
        json!({ "text": format!("/start inv_{code}") }),
    )]);
    hub.until("the guest waits, the owner is asked", |telegram| {
        telegram
            .said(private(GUEST), people::INVITE_RECEIVED)
            .is_some()
            && telegram
                .said(private(OWNER), "Добавить «Гость (@guest)» в cctg?")
                .is_some()
    })
    .await;
    hub.confirm("q-add-guest", &owner).await;
    assert!(hub.allowlist.contains(GUEST));
    hub.batch(vec![in_group(
        5,
        thread,
        &guest,
        &format!("@{BOT} здравствуйте"),
    )]);
    let (content, _) = agent
        .inbound_within(WAIT)
        .await
        .expect("the guest reaches the session");
    assert!(content.contains("здравствуйте"), "{content}");
    // The code worked once.
    hub.batch(vec![in_private(
        2,
        &user(GUEST + 1, "Второй", "second"),
        json!({ "text": format!("/start inv_{code}") }),
    )]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(hub.telegram.general(private(GUEST + 1)).is_empty());

    // The members are in registry.json by now; no id went into a group
    // message.
    for shown in hub.telegram.all() {
        if shown.chat == GROUP {
            for id in [OWNER, BORIS, SILENT, GUEST] {
                assert!(!shown.text.contains(&id.to_string()), "{shown:?}");
            }
        }
    }
}

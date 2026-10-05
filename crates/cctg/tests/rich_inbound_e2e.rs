//! Inbound rich messages end to end (TASK-079): Telegram Desktop 7.x sends a
//! rich message, which has no `text`, only `rich_message.blocks`. An update
//! of that shape ([`FIXTURE`]) goes as a raw `getUpdates` batch through the real
//! update poll ([`updates::poll_until`]) and the real `Slots` actor to an
//! agent over a real TCP link to the real `serve_agents`; Telegram is a
//! fake Bot API. The session reads the markdown of the blocks, signed with
//! its author's name in a team (TASK-036), gathered with the next message
//! into one burst (TASK-070); in the group topic only a mention reaches it,
//! without the bot's name (TASK-077/080), and a reply to a rich message
//! quotes its words (TASK-075).

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::buffer::MENTION_MARK;
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::config::Allowlist;
use cctg::hub::groups::KnownGroups;
use cctg::hub::ingress::{bind, serve_agents};
use cctg::hub::offset::OffsetStore;
use cctg::hub::registry::{ICON_ALIVE, RegistryStore};
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{Control, MentionBot, Options, Owners, Slots};
use cctg::hub::updates::{self, Routed, UpdateSource};
use cctg::wire::{self, AgentMsg, HookEvent, HookPost, HubMsg, Register, Secret};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A user's rich message as a private chat with the bot delivers it, ids and
/// names as placeholders (the test sets its own). No live sample from
/// Telegram Desktop came (2026-10-05): built from the documented `Message`
/// and `RichMessage` and the blocks Telegram returned in probe TASK-075 R1
/// (2026-09-28): the same object type, with `align`/`valign` on every cell.
const FIXTURE: &str = include_str!("fixtures/rich/rich-message.json");
/// What the session reads of [`FIXTURE`]'s blocks.
const FIXTURE_MARKDOWN: &str = "Проверь **таблицу** и `cargo test`:\n\n\
| Файл | Строк |\n| --- | ---: |\n| `updates.rs` | 2220 |\n\n\
- первое\n- *второе*\n\n\
```sh\ncargo test -p cctg\n```";

const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));
const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const CWD: &str = "C:/qa/rich";
const SESSION: &str = "0a16e2e0-0000-4000-8000-000000000079";
const OWNER: i64 = 7_319_402_518;
const BORIS: i64 = 7_319_402_601;
const BOT_ID: i64 = 8_100_200_300;
const BOT: &str = "cctg_test_bot";
const WAIT: Duration = Duration::from_secs(30);

fn private(user: i64) -> Chat {
    Chat::Private(PrivateChat::of_user(user))
}

// ---------------------------------------------------------------- fake Telegram

#[derive(Default)]
struct Telegram {
    ops: Mutex<Vec<Op>>,
    next_message: Mutex<HashMap<Chat, i64>>,
    next_topic: Mutex<HashMap<Chat, i64>>,
}

impl Transport for Telegram {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        let Some(chat) = op.chat() else {
            return Ok(Outcome::Done);
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
            Op::Send { .. } | Op::Stream { into: None, .. } => {
                let mut next = self.next_message.lock().unwrap();
                let id = next.entry(chat).or_insert(1000);
                *id += 1;
                Ok(Outcome::Sent(Message {
                    message_id: *id,
                    ..Message::default()
                }))
            }
            Op::Stream { into: Some(id), .. } => Ok(Outcome::Sent(Message {
                message_id: *id,
                ..Message::default()
            })),
            _ => Ok(Outcome::Done),
        }
    }
}

impl Telegram {
    /// The topic of the slot in `chat`, once made.
    fn topic(&self, chat: Chat) -> Option<i64> {
        self.ops.lock().unwrap().iter().find_map(|op| match op {
            Op::CreateTopic { chat: to, .. } if *to == chat => {
                self.next_topic.lock().unwrap().get(&chat).copied()
            }
            _ => None,
        })
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

/// The recorded rich message as message `message_id` of `from` in topic
/// `thread` of `chat`.
fn recorded(message_id: i64, chat: Chat, thread: i64, from: &Value) -> Value {
    let mut update: Value = serde_json::from_str(FIXTURE).unwrap();
    let message = &mut update["message"];
    assert!(message["rich_message"]["blocks"].is_array(), "{FIXTURE}");
    assert!(message.get("text").is_none(), "a rich message has no text");
    message["message_id"] = json!(message_id);
    message["from"] = from.clone();
    message["chat"] = chat_json(chat);
    message["message_thread_id"] = json!(thread);
    message["is_topic_message"] = json!(true);
    json!({ "message": message })
}

/// Message `message_id` of `from` in topic `thread` of `chat` with `extra`
/// fields (`text`, `rich_message`, `reply_to_message`...).
fn topic_message(message_id: i64, chat: Chat, thread: i64, from: &Value, extra: Value) -> Value {
    let mut message = json!({
        "message_id": message_id, "date": 1, "from": from, "chat": chat_json(chat),
        "message_thread_id": thread, "is_topic_message": true,
    });
    if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    json!({ "message": message })
}

fn chat_json(chat: Chat) -> Value {
    match chat {
        Chat::Group(_) => json!({ "id": GROUP_ID, "type": "supergroup", "is_forum": true }),
        Chat::Private(_) => json!({ "id": OWNER, "type": "private", "first_name": "Анна" }),
    }
}

/// A rich message of one paragraph: `parts` as its rich text.
fn paragraph(parts: Value) -> Value {
    json!({ "rich_message": { "blocks": [{ "type": "paragraph", "text": parts }] } })
}

// ---------------------------------------------------------------- hub

struct Hub {
    telegram: Arc<Telegram>,
    batches: mpsc::UnboundedSender<Vec<Value>>,
    next_update: Mutex<i64>,
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
    let state = std::env::temp_dir().join(format!("cctg-rich-e2e-{}", std::process::id()));
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
    // Two people: a team, each message signed (TASK-036).
    let allowlist: Allowlist = [OWNER, BORIS].into_iter().collect();
    let options = Options {
        grace: Duration::ZERO,
        gather_quiet: Duration::from_millis(400),
        gather_max: Duration::from_secs(6),
        owners: Some(Owners {
            first: PrivateChat::of_user(OWNER),
            devices: None,
            share_new: true,
        }),
        mentions: Some(MentionBot {
            id: BOT_ID,
            username: BOT.into(),
        }),
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
        updates::poll_until(
            &source,
            &groups,
            &allowlist,
            true,
            &offsets,
            move |routed| {
                if let Routed::Input(input) = routed {
                    let _ = control.send(Control::Message(input));
                }
            },
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
            panic!("{what}: {:#?}", self.telegram.ops.lock().unwrap());
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
}

// ---------------------------------------------------------------- agent

struct Agent {
    reader: BufReader<OwnedReadHalf>,
    /// Kept open: a dropped write half ends the link.
    _write: OwnedWriteHalf,
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
            console_line_chars: 0,
            client: None,
            files: false,
            session_reads: false,
            status_lines: false,
            private_place: true,
            enrolled: None,
            heartbeat: false,
            sandbox: None,
        });
        wire::write_msg(&mut write, &register).await.unwrap();
        let mut agent = Self {
            reader: BufReader::new(read),
            _write: write,
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
}

// ---------------------------------------------------------------- the test

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rich_message_reaches_the_session_as_markdown() {
    let hub = start_hub().await;
    let anna = user(OWNER, "Анна", "anna");
    let boris = user(BORIS, "Борис", "boris");
    hub.start_session().await;
    let mut agent = Agent::connect(&hub).await;
    hub.until("the private and the group topic", |telegram| {
        telegram.topic(private(OWNER)).is_some() && telegram.topic(GROUP).is_some()
    })
    .await;
    let mine = hub.telegram.topic(private(OWNER)).unwrap();
    let shared = hub.telegram.topic(GROUP).unwrap();
    while agent
        .inbound_within(Duration::from_millis(300))
        .await
        .is_some()
    {}

    // 1. In the private topic: the recorded rich message and a short one
    // right after it go as one burst, each part signed.
    hub.batch(vec![
        recorded(21, private(OWNER), mine, &anna),
        topic_message(
            22,
            private(OWNER),
            mine,
            &anna,
            paragraph(json!([{ "type": "bold", "text": "ещё" }, " одно"])),
        ),
    ]);
    let (content, meta) = agent.inbound_within(WAIT).await.expect("the burst");
    assert_eq!(
        content,
        format!("anna: {FIXTURE_MARKDOWN}\n\n---\n\nanna: **ещё** одно")
    );
    assert_eq!(meta.get("from_name").map(String::as_str), Some("anna"));
    assert_eq!(meta.get("message_ids").map(String::as_str), Some("21,22"));

    // 2. A reply to a rich message (the bot's answer) quotes its words.
    let answer = json!({
        "message_id": 900, "date": 1, "from": { "id": BOT_ID, "is_bot": true, "first_name": "bot" },
        "chat": chat_json(private(OWNER)), "message_thread_id": mine, "is_topic_message": true,
        "rich_message": { "blocks": [
            { "type": "heading", "text": "Итог", "size": 2 },
            { "type": "list", "items": [{ "label": "•", "blocks": [
                { "type": "paragraph", "text": [{ "type": "code", "text": "api.rs" }, " готов"] }] }] },
        ] },
    });
    hub.batch(vec![topic_message(
        23,
        private(OWNER),
        mine,
        &anna,
        json!({ "text": "отлично", "reply_to_message": answer }),
    )]);
    let (content, meta) = agent.inbound_within(WAIT).await.expect("the reply");
    assert_eq!(content, "> Итог\n> api.rs готов\n\nanna: отлично");
    assert_eq!(
        meta.get("reply_to_message_id").map(String::as_str),
        Some("900")
    );

    // 3. In the group topic a rich message without a mention is kept, not
    // handed over; a rich mention brings it along as history and reaches
    // the session without the bot's name.
    hub.batch(vec![recorded(31, GROUP, shared, &boris)]);
    assert!(
        agent
            .inbound_within(Duration::from_millis(1500))
            .await
            .is_none(),
        "no mention: kept"
    );
    hub.batch(vec![topic_message(
        32,
        GROUP,
        shared,
        &boris,
        paragraph(json!([
            { "type": "mention", "text": format!("@{BOT}"), "username": BOT },
            ", глянь ",
            { "type": "italic", "text": "таблицу" },
        ])),
    )]);
    let (content, meta) = agent.inbound_within(WAIT).await.expect("the mention");
    assert!(
        content.ends_with(&format!("{MENTION_MARK}\nboris: глянь *таблицу*")),
        "{content}"
    );
    assert!(!content.contains(&format!("@{BOT}")), "{content}");
    let first_line = FIXTURE_MARKDOWN.lines().next().unwrap();
    assert!(content.contains(first_line), "the history: {content}");
    assert_eq!(meta.get("mention").map(String::as_str), Some("true"));
    assert_eq!(meta.get("from_name").map(String::as_str), Some("boris"));
}

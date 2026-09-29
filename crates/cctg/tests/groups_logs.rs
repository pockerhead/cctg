//! Log capture for the group paths of the slot actor (TASK-069 review 5):
//! a group joined, left and back, one a stranger added the bot to, a plain
//! group, `/connect` answered and failed, the start check of known groups
//! (one refused with 403) and a share through the group picker. Group ids,
//! group titles, the owner's user id and Telegram's error texts never reach
//! the logs. Its own test binary with a global subscriber: parallel tests
//! would race on tracing callsite registration.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ChatInfo, ChatMember, ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::groups::{GroupLookup, KnownGroups};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{Control, MentionBot, Options, Owners, Slots};
use cctg::hub::updates::{CallbackInput, ConnectInput, MemberUpdate};
use cctg::wire::{HookEvent, HookPost};
use tokio::sync::mpsc;

/// Every id of this test shares these digits, so one search finds any.
const MARK: &str = "9871234";
const DEFAULT: i64 = -1_009_871_234_500;
const KNOWN: i64 = -1_009_871_234_501;
const REFUSED: i64 = -1_009_871_234_502;
const JOINED: i64 = -1_009_871_234_503;
const STRANGERS: i64 = -1_009_871_234_504;
const CONNECTED: i64 = -1_009_871_234_505;
const FAILED: i64 = -1_009_871_234_506;
const PLAIN: i64 = -1_009_871_234_507;
const OWNER: i64 = 7_319_402_518;
const TITLE: &str = "TitleSecret";
const DESCRIPTION: &str = "DescriptionSecret";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut out) = self.0.lock() {
            out.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("logs")).into_owned()
    }
}

/// Takes every call; topics are numbered from 100 in every chat.
#[derive(Default)]
struct Telegram {
    topics: Mutex<Vec<(Chat, i64)>>,
}

impl Transport for Telegram {
    async fn execute(&self, op: &Op) -> Delivery {
        match op {
            Op::CreateTopic { chat, name, .. } => {
                let mut topics = self.topics.lock().expect("topics");
                let id = 100 + i64::try_from(topics.len()).unwrap_or(0);
                topics.push((*chat, id));
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: id,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send { .. } => Ok(Outcome::Sent(Message {
                message_id: 900,
                ..Message::default()
            })),
            _ => Ok(Outcome::Done),
        }
    }
}

fn refused(code: i64) -> ApiError {
    ApiError::Telegram {
        code,
        description: format!("Forbidden: chat {REFUSED} {TITLE} {DESCRIPTION}"),
    }
}

fn admin() -> ChatMember {
    ChatMember {
        status: "administrator".into(),
        can_manage_topics: true,
        can_delete_messages: true,
        ..ChatMember::default()
    }
}

/// Telegram about groups: `REFUSED` refuses with 403, `FAILED` with 500,
/// every other group is a forum supergroup with the bot an administrator.
struct Lookup;

impl GroupLookup for Lookup {
    async fn member(&self, chat: GroupChat) -> Result<ChatMember, ApiError> {
        if chat == GroupChat::of(REFUSED) {
            return Err(refused(403));
        }
        if chat == GroupChat::of(FAILED) {
            return Err(refused(500));
        }
        Ok(admin())
    }

    async fn info(&self, chat: GroupChat) -> Result<ChatInfo, ApiError> {
        if chat == GroupChat::of(REFUSED) {
            return Err(refused(403));
        }
        Ok(ChatInfo {
            kind: "supergroup".into(),
            title: Some(TITLE.into()),
            is_forum: true,
        })
    }

    async fn leave(&self, _: GroupChat) -> Result<(), ApiError> {
        Err(refused(400))
    }
}

fn member(id: i64, status: &str, supergroup: bool, by_allowed: bool) -> Control {
    Control::Member(MemberUpdate {
        chat: GroupChat::of(id),
        supergroup,
        title: Some(TITLE.into()),
        is_forum: supergroup,
        member: ChatMember {
            status: status.into(),
            ..admin()
        },
        by_allowed,
    })
}

fn connect(id: i64) -> Control {
    Control::Connect(ConnectInput {
        chat: GroupChat::of(id),
        supergroup: true,
        title: Some(TITLE.into()),
        is_forum: true,
        thread_id: Some(5),
        target: Some("cctg_bot".into()),
    })
}

async fn until(what: &str, ready: impl Fn() -> bool) {
    let waited = async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), waited)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_logs_carry_no_group_ids_titles_or_users() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let state = std::env::temp_dir().join(format!("cctg-groups-logs-{pid}"));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).expect("state dir");

    let telegram = Arc::new(Telegram::default());
    let fast = Limits::from(BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    });
    let outbox = Outbox::per_chat(telegram.clone(), fast, fast);
    let store = RegistryStore::open(&state).expect("store");
    let mut registry = store.load(GroupChat::of(DEFAULT)).expect("load");
    registry.join_group(GroupChat::of(KNOWN), Some(TITLE.into()), true, true);
    registry.join_group(GroupChat::of(REFUSED), Some(TITLE.into()), true, true);
    let owner = Chat::Private(PrivateChat::of_user(OWNER));
    let options = Options {
        grace: Duration::ZERO,
        owners: Some(Owners {
            first: PrivateChat::of_user(OWNER),
            devices: None,
            share_new: false,
        }),
        mentions: Some(MentionBot {
            id: 8_100_200_300,
            username: "cctg_bot".into(),
        }),
        groups: KnownGroups::default(),
        ..Options::default()
    };
    let mut slots = Slots::new(registry, store, outbox, options);
    slots.look_up_groups(Arc::new(Lookup));
    let (_agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    // A session in the owner's private chat, its topic made.
    hooks
        .send(HookPost::new(
            "box".into(),
            "9e0a1b2c-0000-4000-8000-000000000069".into(),
            "C:/qa/groups".into(),
            String::new(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await
        .expect("hook");
    let private_topic = || {
        telegram
            .topics
            .lock()
            .expect("topics")
            .iter()
            .find(|(chat, _)| *chat == owner)
            .map(|(_, id)| *id)
    };
    until("the private topic", || private_topic().is_some()).await;

    let send = |message| control.send(message).expect("actor");
    send(member(JOINED, "administrator", true, true));
    send(member(STRANGERS, "administrator", true, false));
    send(member(PLAIN, "administrator", false, true));
    send(member(KNOWN, "kicked", true, false));
    send(connect(CONNECTED));
    send(connect(FAILED));
    // The owner shares the session into the group just joined; pressed
    // again until the actor knows the private topic (its answer from
    // Telegram and the press come on different channels).
    let press = || {
        send(Control::Callback(CallbackInput {
            chat: Some(owner),
            query_id: "q-pick".into(),
            data: Some(format!("grp:0:{JOINED}:s")),
            message_id: Some(900),
            thread_id: private_topic(),
            from_name: None,
            display_name: Some(TITLE.into()),
            sender: PrivateChat::of_user(OWNER),
        }))
    };
    let shared = async {
        while !captured.text().contains("slot shared to a group") {
            press();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), shared)
        .await
        .expect("the share");
    let wanted = [
        "group connected",
        "outside the allowlist",
        "the bot left a group",
        "the bot is no longer in a group",
        "/connect could not be checked",
        "slot shared to a group",
    ];
    until("every group path logged", || {
        let logs = captured.text();
        wanted.iter().all(|line| logs.contains(line))
    })
    .await;
    until("the shared topic in the joined group", || {
        telegram
            .topics
            .lock()
            .expect("topics")
            .iter()
            .any(|(chat, _)| *chat == Chat::Group(GroupChat::of(JOINED)))
    })
    .await;
    // What the last calls log, too.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let logs = captured.text();
    for secret in [MARK, TITLE, DESCRIPTION, &OWNER.to_string()] {
        assert!(!logs.contains(secret), "{secret} in the logs:\n{logs}");
    }
    let _ = std::fs::remove_dir_all(&state);
}

//! Log capture for the people of the menu (TASK-081): a person added by a
//! forward, one by an invite link, one removed again, with the update poll's
//! classification of every update, at debug level. No user id, name,
//! username, invite code or link reaches the logs. Its own test binary with
//! a global subscriber: parallel tests would race on tracing callsite
//! registration.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::config::Allowlist;
use cctg::hub::groups::KnownGroups;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Limits, Op, Outbox, Outcome, Transport};
use cctg::hub::slots::{Control, MentionBot, Options, Owners, Slots};
use cctg::hub::updates::{self, Routed};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Every id of this test shares these digits, so one search finds any.
const MARK: &str = "7654321";
const GROUP_ID: i64 = -1_007_654_321_000;
const OWNER: i64 = 7_654_321_001;
const ZINA: i64 = 7_654_321_002;
const GUEST: i64 = 7_654_321_003;
const NAMES: [&str; 6] = [
    "ВладелицаСекрет",
    "ЗинаидаСекрет",
    "zina_secret",
    "ГостьяСекрет",
    "guest_secret",
    "owner_secret",
];

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

/// A sent message: its chat, id, text and keyboard.
type Sent = (Chat, i64, String, Option<Value>);

/// Takes every call; keeps what was sent to read buttons and links back.
/// Zina's private chat refuses the bot (403).
#[derive(Default)]
struct Telegram {
    sent: Mutex<Vec<Sent>>,
    next: Mutex<i64>,
}

impl Transport for Telegram {
    async fn execute(&self, op: &Op) -> Delivery {
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send {
                chat,
                text,
                reply_markup,
                ..
            } => {
                let id = {
                    let mut next = self.next.lock().expect("ids");
                    *next += 1;
                    900 + *next
                };
                self.sent.lock().expect("sent").push((
                    *chat,
                    id,
                    text.clone(),
                    reply_markup.clone(),
                ));
                if *chat == Chat::Private(PrivateChat::of_user(ZINA)) {
                    return Err(cctg::hub::api::ApiError::Telegram {
                        code: 403,
                        description: format!("Forbidden: user {ZINA} ЗинаидаСекрет"),
                    });
                }
                Ok(Outcome::Sent(Message {
                    message_id: id,
                    ..Message::default()
                }))
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Telegram {
    /// The newest message to `chat` that says `text`: its id, text and
    /// callback data.
    fn said(&self, chat: Chat, text: &str) -> Option<(i64, String, Vec<String>)> {
        let sent = self.sent.lock().expect("sent");
        sent.iter()
            .rev()
            .find(|(to, _, said, _)| *to == chat && said.contains(text))
            .map(|(_, id, said, markup)| {
                let datas = markup
                    .as_ref()
                    .and_then(|markup| markup["inline_keyboard"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_array)
                    .flatten()
                    .filter_map(|button| button["callback_data"].as_str().map(str::to_owned))
                    .collect();
                (*id, said.clone(), datas)
            })
    }
}

fn user(id: i64, first_name: &str, username: &str) -> Value {
    json!({ "id": id, "is_bot": false, "first_name": first_name, "username": username })
}

fn private_message(id: i64, from: &Value, extra: Value) -> Value {
    let mut message = json!({
        "message_id": id, "date": 1, "from": from,
        "chat": { "id": from["id"], "type": "private" },
    });
    if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    json!({ "update_id": id, "message": message })
}

fn owner_press(id: i64, message_id: i64, data: &str) -> Value {
    json!({ "update_id": id, "callback_query": {
        "id": format!("q{id}"), "from": user(OWNER, "ВладелицаСекрет", "owner_secret"),
        "chat_instance": "c", "data": data,
        "message": { "message_id": message_id, "date": 1, "chat": { "id": OWNER, "type": "private" } },
    }})
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
async fn people_logs_carry_no_ids_names_or_codes() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let state = std::env::temp_dir().join(format!("cctg-people-logs-{}", std::process::id()));
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
    let registry = store.load(GroupChat::of(GROUP_ID)).expect("load");
    let groups = KnownGroups::default();
    let allowlist: Allowlist = [OWNER].into_iter().collect();
    let options = Options {
        grace: Duration::ZERO,
        owners: Some(Owners {
            first: PrivateChat::of_user(OWNER),
            devices: None,
            share_new: false,
        }),
        menu: true,
        mentions: Some(MentionBot {
            id: 42,
            username: "cctg_test_bot".into(),
        }),
        groups: groups.clone(),
        allowlist: allowlist.clone(),
        ..Options::default()
    };
    let slots = Slots::new(registry, store, outbox, options);
    let (_agents, agents_rx) = mpsc::channel(4);
    let (_hooks, hooks_rx) = mpsc::channel(4);
    let (control, control_rx) = mpsc::unbounded_channel();
    let actor = tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    // Updates as the poll classifies them, with its debug lines.
    let feed = |update: Value| {
        let (_, routed) = updates::route_batch_with(vec![update], None, &groups, &allowlist, true);
        for routed in routed {
            let control_msg = match routed {
                Routed::Input(input) => Control::Message(input),
                Routed::Callback(input) => Control::Callback(input),
                Routed::Forward(input) => Control::Forward(input),
                Routed::Invite(input) => Control::Invite(input),
                _ => continue,
            };
            control.send(control_msg).expect("the actor");
        }
    };
    let owner = user(OWNER, "ВладелицаСекрет", "owner_secret");
    let zina = user(ZINA, "ЗинаидаСекрет", "zina_secret");
    let guest = user(GUEST, "ГостьяСекрет", "guest_secret");
    let private = |id| Chat::Private(PrivateChat::of_user(id));

    // A stranger first: the poll drops her.
    feed(private_message(1, &zina, json!({ "text": "привет" })));
    // Zina added by a forward; her greeting gets 403.
    feed(private_message(
        2,
        &owner,
        json!({ "text": "её слова", "forward_origin": { "type": "user", "date": 1, "sender_user": zina } }),
    ));
    until("the proposal", || {
        telegram.said(private(OWNER), "в cctg?").is_some()
    })
    .await;
    let (proposal, _, datas) = telegram.said(private(OWNER), "в cctg?").unwrap();
    feed(owner_press(3, proposal, &datas[0]));
    until("added", || allowlist.contains(ZINA)).await;
    // The guest by an invite link.
    feed(private_message(
        4,
        &owner,
        json!({ "text": "скрыт", "forward_origin": { "type": "hidden_user", "date": 1, "sender_user_name": "ГостьяСекрет" } }),
    ));
    until("the hidden answer", || {
        telegram.said(private(OWNER), "скрывает").is_some()
    })
    .await;
    let (hint, _, _) = telegram.said(private(OWNER), "скрывает").unwrap();
    feed(owner_press(5, hint, "menu:pi"));
    until("the link", || {
        telegram.said(private(OWNER), "start=inv_").is_some()
    })
    .await;
    let (_, link, _) = telegram.said(private(OWNER), "start=inv_").unwrap();
    let code = link
        .split("start=inv_")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("a code")
        .to_owned();
    feed(private_message(
        6,
        &guest,
        json!({ "text": format!("/start inv_{code}") }),
    ));
    feed(private_message(
        7,
        &guest,
        json!({ "text": format!("/start inv_{code}") }),
    ));
    until("the guest's proposal", || {
        telegram.said(private(OWNER), "ГостьяСекрет").is_some()
    })
    .await;
    let (proposal, _, datas) = telegram.said(private(OWNER), "ГостьяСекрет").unwrap();
    feed(owner_press(8, proposal, &datas[0]));
    until("the guest added", || allowlist.contains(GUEST)).await;
    // Zina removed with two presses on the owner's menu.
    feed(private_message(9, &owner, json!({ "text": "/menu" })));
    until("the menu", || {
        telegram
            .said(private(OWNER), "Сессии")
            .is_some_and(|(_, _, datas)| datas.contains(&"menu:pp".to_owned()))
    })
    .await;
    let (menu, _, _) = telegram.said(private(OWNER), "Сессии").unwrap();
    // The actor learns the menu's id from Telegram's answer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    feed(owner_press(10, menu, "menu:pr:1"));
    feed(owner_press(11, menu, "menu:prc:1"));
    until("removed", || !allowlist.contains(ZINA)).await;
    // Hers after that: dropped.
    feed(private_message(12, &zina, json!({ "text": "а я?" })));
    until("every people path logged", || {
        let logs = captured.text();
        [
            "person added from the menu",
            "invite link minted",
            "invite code not valid",
            "people message not delivered",
            "person removed from the menu",
        ]
        .iter()
        .all(|line| logs.contains(line))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    control.send(Control::Stop).expect("the actor");
    let _ = tokio::time::timeout(Duration::from_secs(10), actor).await;

    let logs = captured.text();
    for secret in [MARK, &code, "start=inv_", "t.me/"]
        .into_iter()
        .chain(NAMES)
    {
        assert!(!logs.contains(secret), "{secret} in the logs:\n{logs}");
    }
    let _ = std::fs::remove_dir_all(&state);
}

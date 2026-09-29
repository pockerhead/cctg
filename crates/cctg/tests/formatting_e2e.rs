//! TASK-027: a formatted message through the real `BotApi` and `Scheduler`
//! against a fake Telegram HTTP server that cannot parse HTML.
//! TASK-041: the same server shows which sends go without a sound.

use std::sync::{Arc, Mutex};

use cctg::hub::api::{BotApi, Document};
use cctg::hub::chat::{Chat, GroupChat, PrivateChat};
use cctg::hub::config::Config;
use cctg::hub::scheduler::{BucketConfig, Op, Outbox, Outcome, Scheduler};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// The default group of this test (TASK-069): the chat its Bot API fake
/// and its registry name.
const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));

const CANT_PARSE: &str = r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Unsupported start tag \"x\" at byte offset 0"}"#;
const SENT: &str = r#"{"ok":true,"result":{"message_id":7,"chat":{"id":-1001}}}"#;

/// Answers `sendMessage` with `parse_mode` as unparsable and every other
/// request as sent; records the JSON bodies (a body that is not JSON, like a
/// multipart document, as a string).
async fn fake_telegram(bodies: Arc<Mutex<Vec<Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let bodies = bodies.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                loop {
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    if read.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    let body: Value = serde_json::from_slice(&body).unwrap_or_else(|_| {
                        Value::String(String::from_utf8_lossy(&body).into_owned())
                    });
                    let (status, answer) = if body.get("parse_mode").is_some() {
                        ("400 Bad Request", CANT_PARSE)
                    } else {
                        ("200 OK", SENT)
                    };
                    bodies.lock().unwrap().push(body);
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{answer}",
                        answer.len()
                    );
                    if write.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

/// A scheduler on the real `BotApi` against [`fake_telegram`].
async fn scheduler(bodies: Arc<Mutex<Vec<Value>>>) -> (Scheduler<BotApi>, Outbox) {
    let url = fake_telegram(bodies).await;
    let token = Config::from_vars(|name| match name {
        "CCTG_BOT_TOKEN" => Some("1:test".to_owned()),
        "CCTG_CHAT_ID" => Some("-1001".to_owned()),
        "CCTG_ALLOWED_USER_IDS" => Some("1".to_owned()),
        _ => None,
    })
    .unwrap()
    .token;
    let api = Arc::new(BotApi::with_api_url(&url, &token, None).unwrap());
    let fast = BucketConfig {
        capacity: 100,
        refill_every: std::time::Duration::from_millis(1),
        min_gap: std::time::Duration::ZERO,
    };
    Scheduler::new(api, fast)
}

#[tokio::test]
async fn html_refused_by_telegram_is_sent_again_once_as_plain_text() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(bodies.clone()).await;
    let running = tokio::spawn(scheduler.run());
    let answer = outbox
        .submit(Op::Send {
            chat: GROUP,
            thread_id: Some(5),
            text: "**done** <ok>".to_owned(),
            html: Some("<b>done</b> &lt;ok&gt;".to_owned()),
            rich: None,
            reply_markup: None,
            permission: false,
            reply_to: None,
            notify: true,
        })
        .await;
    let delivery = answer.await.unwrap();
    assert!(matches!(delivery, Ok(Outcome::Sent(ref message)) if message.message_id == 7));
    drop(outbox);
    running.await.unwrap();

    let bodies = bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[0]["parse_mode"], "HTML");
    assert_eq!(bodies[0]["text"], "<b>done</b> &lt;ok&gt;");
    assert_eq!(bodies[0]["message_thread_id"], 5);
    assert!(bodies[1].get("parse_mode").is_none());
    assert_eq!(bodies[1]["text"], "**done** <ok>");
    assert_eq!(bodies[1]["message_thread_id"], 5);
}

#[tokio::test]
async fn only_loud_sends_go_without_disable_notification() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(bodies.clone()).await;
    let running = tokio::spawn(scheduler.run());
    let send = |text: &str, notify| Op::Send {
        chat: GROUP,
        thread_id: Some(5),
        text: text.to_owned(),
        html: None,
        rich: None,
        reply_markup: None,
        permission: false,
        reply_to: None,
        notify,
    };
    let line = |text: &str, notify| Op::Stream {
        chat: GROUP,
        thread_id: 5,
        text: text.to_owned(),
        html: None,
        rich: None,
        merge: false,
        restart: false,
        notify,
        into: None,
    };
    let document = |name: &str, notify| Op::SendDocument {
        chat: GROUP,
        thread_id: Some(5),
        document: Document {
            file_name: name.to_owned(),
            bytes: b"body".to_vec(),
            caption: None,
        },
        notify,
    };
    let ops = [
        send("quiet send", false),
        send("loud send", true),
        line("quiet line", false),
        line("loud line", true),
        document("quiet.txt", false),
        document("loud.txt", true),
    ];
    for op in ops {
        let delivery = outbox.submit(op).await.await.unwrap();
        assert!(matches!(delivery, Ok(Outcome::Sent(_))), "{delivery:?}");
    }
    drop(outbox);
    running.await.unwrap();

    let bodies = bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 6, "{bodies:?}");
    let json = |index: usize, text: &str, quiet: bool| {
        let body = &bodies[index];
        assert_eq!(body["text"], text);
        if quiet {
            assert_eq!(body["disable_notification"], true, "{body}");
        } else {
            assert!(body.get("disable_notification").is_none(), "{body}");
        }
    };
    json(0, "quiet send", true);
    json(1, "loud send", false);
    json(2, "quiet line", true);
    json(3, "loud line", false);
    let multipart = |index: usize, name: &str, quiet: bool| {
        let body = bodies[index].as_str().unwrap();
        assert!(body.contains(&format!("filename=\"{name}\"")), "{body}");
        let field = "name=\"disable_notification\"";
        assert_eq!(body.contains(field), quiet, "{body}");
        if quiet {
            let value = body.split(field).nth(1).unwrap();
            assert!(value.trim_start().starts_with("true"), "{body}");
        }
    };
    multipart(4, "quiet.txt", true);
    multipart(5, "loud.txt", false);
}

/// TASK-061: every request names its chat: the configured group for
/// `GROUP`, the user's id for the private chat with that user. The same
/// message id goes to each chat as asked.
#[tokio::test]
async fn every_request_goes_to_the_chat_its_op_names() {
    const USER: i64 = 7_319_402_518;
    let private = Chat::Private(PrivateChat::of_user(USER));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(bodies.clone()).await;
    let running = tokio::spawn(scheduler.run());
    let send = |chat| Op::Send {
        chat,
        thread_id: Some(5),
        text: "hi".to_owned(),
        html: None,
        rich: None,
        reply_markup: None,
        permission: false,
        reply_to: None,
        notify: false,
    };
    let edit = |chat| Op::Edit {
        chat,
        message_id: 9,
        text: "edited".to_owned(),
        reply_markup: None,
        background: false,
    };
    let react = |chat| Op::React {
        chat,
        message_id: 9,
        emoji: "👀".to_owned(),
    };
    let topic = |chat| Op::EditTopic {
        chat,
        thread_id: 5,
        name: Some("name".to_owned()),
        icon_custom_emoji_id: None,
    };
    let document = |chat| Op::SendDocument {
        chat,
        thread_id: Some(5),
        document: Document {
            file_name: "a.txt".to_owned(),
            bytes: b"body".to_vec(),
            caption: None,
        },
        notify: false,
    };
    for chat in [GROUP, private] {
        for op in [
            send(chat),
            edit(chat),
            react(chat),
            topic(chat),
            document(chat),
        ] {
            assert!(outbox.submit(op).await.await.unwrap().is_ok());
        }
    }
    drop(outbox);
    running.await.unwrap();

    let bodies = bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 10, "{bodies:?}");
    for (index, body) in bodies.iter().enumerate() {
        let want = if index < 5 { -1001 } else { USER };
        match body.as_str() {
            // The multipart document: its chat_id field.
            Some(form) => {
                let value = form.split("name=\"chat_id\"").nth(1).unwrap();
                assert!(
                    value.trim_start().starts_with(&want.to_string()),
                    "{index}: {form}"
                );
            }
            None => assert_eq!(body["chat_id"], want, "{index}: {body}"),
        }
    }
}

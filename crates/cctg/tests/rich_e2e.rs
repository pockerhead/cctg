//! TASK-075: rich messages through the real `BotApi` and `Scheduler`
//! against a fake Telegram HTTP server: the shape of `sendRichMessage` and
//! of a rich `editMessageText`, and the fallback to today's HTML messages
//! when Telegram refuses the rich form.
//!
//! With `CCTG_RICH_BODY_OUT` set, the body of the `sendRichMessage` the hub
//! built for the answer below is written there (for a live check on the
//! test bot; the chat id in it is the fake one).

use std::sync::{Arc, Mutex};

use cctg::hub::api::BotApi;
use cctg::hub::chat::{Chat, GroupChat};
use cctg::hub::config::Config;
use cctg::hub::scheduler::{BucketConfig, Op, Outbox, Outcome, Rich, Scheduler};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use transcript::{HtmlChunk, SplitOptions, rich_markdown, split_markdown_for_telegram};

/// The default group of this test (TASK-069): the chat its Bot API fake
/// and its registry name.
const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));

const REFUSED: &str =
    r#"{"ok":false,"error_code":400,"description":"Bad Request: RICH_MESSAGE_BLOCKS_TOO_MANY"}"#;

/// A turn answer as models write it: a heading, a table, a list nested
/// three levels, a fence with a `<` in it, a `<` in the text.
const ANSWER: &str = "# Итог\n\n\
| Файл | Строк | Статус |\n\
|:-----|------:|:------:|\n\
| `crates/hub/api.rs` | 1109 | ✅ |\n\
| `crates/hub/slots.rs` | 29107 | ⚠️ |\n\n\
1. Первый пункт\n   - вложенный\n     - ещё глубже\n\
2. Второй пункт\n\n\
```rust\nlet v: Vec<String> = Vec::new();\n```\n\n\
Проверено: a<b и Vec<u8> остаются текстом.\n";

/// One request the fake got: its Bot API method and JSON body.
type Request = (String, Value);

/// Answers every request as sent (message ids 7, 8, ...), and with
/// `refuse_rich` every rich one (`sendRichMessage`, `editMessageText` with
/// `rich_message`) with a `RICH_MESSAGE_*` 400; records the requests.
async fn fake_telegram(requests: Arc<Mutex<Vec<Request>>>, refuse_rich: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let next_id = Arc::new(Mutex::new(7));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let requests = requests.clone();
            let next_id = next_id.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                loop {
                    let mut method = String::new();
                    let mut length = 0;
                    let mut first = true;
                    loop {
                        let mut line = String::new();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if first {
                            // `POST /bot<token>/<method> HTTP/1.1`
                            let path = line.split(' ').nth(1).unwrap_or_default();
                            method = path.rsplit('/').next().unwrap_or_default().to_owned();
                            first = false;
                            continue;
                        }
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
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let rich = body.get("rich_message").is_some();
                    let answer = if rich && refuse_rich {
                        ("400 Bad Request", REFUSED.to_owned())
                    } else {
                        let id = match body.get("message_id").and_then(Value::as_i64) {
                            Some(id) => id,
                            None => {
                                let mut next = next_id.lock().unwrap();
                                *next += 1;
                                *next - 1
                            }
                        };
                        let result = if method == "editMessageText" {
                            json!(true)
                        } else {
                            json!({ "message_id": id, "chat": { "id": -1001 } })
                        };
                        (
                            "200 OK",
                            json!({ "ok": true, "result": result }).to_string(),
                        )
                    };
                    requests.lock().unwrap().push((method.clone(), body));
                    let response = format!(
                        "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                        answer.0,
                        answer.1.len(),
                        answer.1
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
async fn scheduler(
    requests: Arc<Mutex<Vec<Request>>>,
    refuse_rich: bool,
) -> (Scheduler<BotApi>, Outbox) {
    let url = fake_telegram(requests, refuse_rich).await;
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

/// Today's messages of `text`, all of them.
fn chunks(text: &str) -> Vec<HtmlChunk> {
    split_markdown_for_telegram(
        text,
        SplitOptions {
            max_chunks: usize::MAX,
        },
    )
    .chunks
}

/// `text` as the hub sends an answer in a view with rich messages: one
/// rich message, today's messages for the fallback.
fn rich_send(text: &str, notify: bool) -> Op {
    let mut chunks = chunks(text);
    let last = chunks.pop().unwrap();
    Op::Send {
        chat: GROUP,
        thread_id: Some(5),
        text: last.text,
        html: Some(last.html),
        rich: Some(Box::new(Rich {
            markdown: Some(rich_markdown(text)),
            before: chunks.into_iter().map(|c| (c.text, c.html)).collect(),
            file: None,
        })),
        reply_markup: Some(json!({ "inline_keyboard": [[{ "text": "x", "callback_data": "y" }]] })),
        permission: false,
        reply_to: Some(3),
        notify,
    }
}

#[tokio::test]
async fn an_answer_goes_as_one_send_rich_message_with_its_markdown() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(requests.clone(), false).await;
    let running = tokio::spawn(scheduler.run());
    let loud = outbox.submit(rich_send(ANSWER, true)).await;
    assert!(matches!(loud.await, Ok(Ok(Outcome::Sent(ref m))) if m.message_id == 7));
    let quiet = outbox.submit(rich_send("quiet", false)).await;
    assert!(matches!(quiet.await, Ok(Ok(Outcome::Sent(_)))));
    drop(outbox);
    running.await.unwrap();

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    let (method, body) = &requests[0];
    assert_eq!(method, "sendRichMessage");
    let markdown = body["rich_message"]["markdown"].as_str().unwrap();
    assert_eq!(markdown, rich_markdown(ANSWER));
    // The table, the nested list and the code stay as they are; only a `<`
    // outside code is an entity.
    assert!(markdown.contains("|:-----|------:|:------:|"));
    assert!(markdown.contains("\n     - ещё глубже\n"));
    assert!(markdown.contains("let v: Vec<String> = Vec::new();"));
    assert!(markdown.contains("a&lt;b и Vec&lt;u8>"));
    assert_eq!(body["message_thread_id"], 5);
    assert_eq!(body["reply_parameters"], json!({ "message_id": 3 }));
    assert_eq!(
        body["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        "y"
    );
    for absent in ["text", "parse_mode", "disable_notification"] {
        assert!(body.get(absent).is_none(), "{absent}: {body}");
    }
    let (method, quiet) = &requests[1];
    assert_eq!(method, "sendRichMessage");
    assert_eq!(quiet["disable_notification"], true);
    if let Some(path) = std::env::var_os("CCTG_RICH_BODY_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(body).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn a_refused_rich_answer_goes_as_its_html_messages_without_loss() {
    let answer = format!("{ANSWER}\n{}\n\n{}\n", "a".repeat(3000), "b".repeat(3000));
    let want = chunks(&answer);
    assert!(want.len() >= 2, "{}", want.len());
    let sources: String = want.iter().map(|chunk| chunk.text.as_str()).collect();
    assert_eq!(sources, answer, "the chunks carry the whole text");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(requests.clone(), true).await;
    let running = tokio::spawn(scheduler.run());
    let delivery = outbox.submit(rich_send(&answer, true)).await;
    let delivery = delivery.await.unwrap();
    drop(outbox);
    running.await.unwrap();

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), want.len() + 1, "{requests:#?}");
    assert_eq!(requests[0].0, "sendRichMessage");
    for ((method, body), chunk) in requests[1..].iter().zip(&want) {
        assert_eq!(method, "sendMessage");
        assert_eq!(body["parse_mode"], "HTML");
        assert_eq!(body["text"], chunk.html.as_str());
        assert_eq!(body["message_thread_id"], 5);
        assert!(body.get("rich_message").is_none());
    }
    // Only the last one answers and keeps the buttons and the reply.
    let last = &requests.last().unwrap().1;
    assert!(last.get("reply_markup").is_some() && last.get("reply_parameters").is_some());
    assert!(requests[1].1.get("reply_markup").is_none());
    let last_id = 7 + want.len() as i64 - 1;
    assert!(
        matches!(delivery, Ok(Outcome::Sent(ref m)) if m.message_id == last_id),
        "{delivery:?}"
    );
}

#[tokio::test]
async fn a_rich_write_into_a_message_and_its_html_fallback() {
    let write = || Op::Stream {
        chat: GROUP,
        thread_id: 5,
        text: "Смотрю.\n• Bash: a<b ✓".to_owned(),
        html: Some("Смотрю.\n• Bash: a&lt;b ✓".to_owned()),
        rich: Some(Box::new(Rich {
            markdown: Some("Смотрю.\n\n<p>• Bash: a&lt;b ✓</p>".to_owned()),
            before: Vec::new(),
            file: None,
        })),
        merge: false,
        restart: false,
        notify: false,
        into: Some(40),
    };
    for refuse in [false, true] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (scheduler, outbox) = scheduler(requests.clone(), refuse).await;
        let running = tokio::spawn(scheduler.run());
        let delivery = outbox.submit(write()).await;
        assert!(
            matches!(delivery.await, Ok(Ok(Outcome::Sent(ref m))) if m.message_id == 40),
            "refuse {refuse}"
        );
        drop(outbox);
        running.await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1 + usize::from(refuse), "{requests:#?}");
        let (method, body) = &requests[0];
        assert_eq!(method, "editMessageText");
        assert_eq!(body["message_id"], 40);
        assert_eq!(
            body["rich_message"]["markdown"],
            "Смотрю.\n\n<p>• Bash: a&lt;b ✓</p>"
        );
        assert_eq!(body["reply_markup"], json!({ "inline_keyboard": [] }));
        assert!(body.get("text").is_none() && body.get("parse_mode").is_none());
        if refuse {
            let (method, body) = &requests[1];
            assert_eq!(method, "editMessageText");
            assert_eq!(body["text"], "Смотрю.\n• Bash: a&lt;b ✓");
            assert_eq!(body["parse_mode"], "HTML");
            assert!(body.get("rich_message").is_none());
        }
    }
}

/// A new stream message with its rich form (a streamed turn answer, a new
/// turn message): `sendRichMessage` into its topic, no buttons, no reply,
/// quiet unless it is an answer.
#[tokio::test]
async fn a_new_rich_stream_message_goes_as_send_rich_message() {
    let line = |notify: bool| Op::Stream {
        chat: GROUP,
        thread_id: 5,
        text: "Смотрю.".to_owned(),
        html: Some("Смотрю.".to_owned()),
        rich: Some(Box::new(Rich {
            markdown: Some("Смотрю.\n\n<p>• Bash: a&lt;b ✓</p>".to_owned()),
            before: Vec::new(),
            file: None,
        })),
        merge: false,
        restart: false,
        notify,
        into: None,
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (scheduler, outbox) = scheduler(requests.clone(), false).await;
    let running = tokio::spawn(scheduler.run());
    for notify in [false, true] {
        let delivery = outbox.submit(line(notify)).await;
        assert!(
            matches!(delivery.await, Ok(Ok(Outcome::Sent(_)))),
            "notify {notify}"
        );
    }
    drop(outbox);
    running.await.unwrap();

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    for ((method, body), notify) in requests.iter().zip([false, true]) {
        assert_eq!(method, "sendRichMessage");
        assert_eq!(
            body["rich_message"]["markdown"],
            "Смотрю.\n\n<p>• Bash: a&lt;b ✓</p>"
        );
        assert_eq!(body["message_thread_id"], 5);
        for absent in ["text", "parse_mode", "reply_markup", "reply_parameters"] {
            assert!(body.get(absent).is_none(), "{absent}: {body}");
        }
        assert_eq!(
            body.get("disable_notification").is_some(),
            !notify,
            "{body}"
        );
    }
}

//! TASK-027: a formatted message through the real `BotApi` and `Scheduler`
//! against a fake Telegram HTTP server that cannot parse HTML.

use std::sync::{Arc, Mutex};

use cctg::hub::api::BotApi;
use cctg::hub::config::Config;
use cctg::hub::scheduler::{BucketConfig, Op, Outcome, Scheduler};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const CANT_PARSE: &str = r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: Unsupported start tag \"x\" at byte offset 0"}"#;
const SENT: &str = r#"{"ok":true,"result":{"message_id":7,"chat":{"id":-1001}}}"#;

/// Answers `sendMessage` with `parse_mode` as unparsable and every other
/// request as sent; records the JSON bodies.
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
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
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

#[tokio::test]
async fn html_refused_by_telegram_is_sent_again_once_as_plain_text() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let url = fake_telegram(bodies.clone()).await;
    let token = Config::from_vars(|name| match name {
        "CCTG_BOT_TOKEN" => Some("1:test".to_owned()),
        "CCTG_CHAT_ID" => Some("-1001".to_owned()),
        "CCTG_ALLOWED_USER_IDS" => Some("1".to_owned()),
        _ => None,
    })
    .unwrap()
    .token;
    let api = Arc::new(BotApi::with_api_url(&url, &token, -1001).unwrap());
    let fast = BucketConfig {
        capacity: 100,
        refill_every: std::time::Duration::from_millis(1),
        min_gap: std::time::Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(api, fast);
    let running = tokio::spawn(scheduler.run());
    let answer = outbox
        .submit(Op::Send {
            thread_id: Some(5),
            text: "**done** <ok>".to_owned(),
            html: Some("<b>done</b> &lt;ok&gt;".to_owned()),
            reply_markup: None,
            permission: false,
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

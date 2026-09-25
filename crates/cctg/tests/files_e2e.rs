//! Files both ways between a topic and its session (TASK-032), over the
//! real pieces: the real `BotApi` and `Scheduler` against a fake Telegram
//! HTTP server (getFile, file downloads, sendPhoto, sendDocument), the
//! real `Slots` actor with its download task, the real `serve_agents` TCP
//! link, and the real agent link and channel loop (`agent::spawn`,
//! `serve_channel`) with this test as Claude Code. A second session's agent
//! is a raw wire peer that registers like an agent from before TASK-032.
//!
//! Its own test binary with a global log subscriber: at the end, no file
//! name, caption or file content of either direction is in the logs.

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::agent::{self, Backoff, Dirs, Frame, LinkConfig};
use cctg::channel::Hub;
use cctg::hub::api::BotApi;
use cctg::hub::buffer::{OLD_AGENT_NOTICE, TOO_BIG_NOTICE};
use cctg::hub::config::{Allowlist, Config};
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Scheduler};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{Routed, route_batch};
use cctg::wire::{self, AgentMsg, HookEvent, HookPost, HubMsg, Register, Secret};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SECRET: &str = "files-e2e-secret-0123456789";
const SESSION: &str = "f11e5000-0000-4000-8000-000000000032";
const OLD: &str = "01d00000-0000-4000-8000-000000000032";
const CHAT: i64 = -1000000000001;
const USER: i64 = 7_318_046_259;
const WAIT: Duration = Duration::from_secs(30);

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

/// What the fake Telegram saw: `(method, body)`, the body as text (lossy
/// for uploads).
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Telegram's files by id; `huge` is too big for a bot. Topics are 100 and
/// 101; a photo whose bytes hold `REFUSE-PHOTO` is refused by sendPhoto.
async fn fake_telegram(files: BTreeMap<String, Vec<u8>>, seen: Seen) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(files);
    let topics = Arc::new(Mutex::new(100));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (files, seen, topics) = (files.clone(), seen.clone(), topics.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                loop {
                    let mut request = String::new();
                    if read.read_line(&mut request).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
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
                    let (status, answer) = answer(&path, &body, &files, &seen, &topics);
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\n\r\n",
                        answer.len()
                    );
                    if write.write_all(head.as_bytes()).await.is_err()
                        || write.write_all(&answer).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    url
}

fn answer(
    path: &str,
    body: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
    seen: &Seen,
    topics: &Mutex<i64>,
) -> (&'static str, Vec<u8>) {
    let ok = |result: Value| {
        (
            "200 OK",
            json!({ "ok": true, "result": result })
                .to_string()
                .into_bytes(),
        )
    };
    let refused = |description: &str| {
        let body = json!({ "ok": false, "error_code": 400, "description": description });
        ("400 Bad Request", body.to_string().into_bytes())
    };
    if let Some(file) = path.split("/file/bot").nth(1) {
        let id = file
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_end_matches(".jpg");
        return match files.get(id) {
            Some(bytes) => ("200 OK", bytes.clone()),
            None => ("404 Not Found", b"{}".to_vec()),
        };
    }
    let method = path.rsplit('/').next().unwrap_or_default().to_owned();
    let text = String::from_utf8_lossy(body).into_owned();
    seen.lock().unwrap().push((method.clone(), text.clone()));
    let message = json!({ "message_id": 500, "chat": { "id": CHAT } });
    match method.as_str() {
        "getFile" => {
            let id = serde_json::from_slice::<Value>(body).unwrap()["file_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match files.get(&id) {
                _ if id == "huge" => refused("Bad Request: file is too big"),
                Some(bytes) => ok(json!({ "file_id": id, "file_unique_id": "u",
                    "file_size": bytes.len(), "file_path": format!("photos/{id}.jpg") })),
                None => refused("Bad Request: wrong file_id"),
            }
        }
        "sendPhoto" if text.contains("REFUSE-PHOTO") => {
            refused("Bad Request: PHOTO_INVALID_DIMENSIONS")
        }
        "createForumTopic" => {
            let mut next = topics.lock().unwrap();
            let topic = *next;
            *next += 1;
            ok(json!({ "message_thread_id": topic, "name": "t" }))
        }
        "sendMessage" | "sendPhoto" | "sendDocument" => ok(message),
        _ => ok(json!(true)),
    }
}

/// A topic message from the allowlisted user, through the real update
/// parsing.
fn topic_message(message_id: i64, thread_id: i64, fields: Value) -> Control {
    let mut update = json!({ "update_id": message_id, "message": {
        "message_id": message_id, "message_thread_id": thread_id, "is_topic_message": true,
        "date": 1, "from": { "id": USER, "is_bot": false, "first_name": "x" },
        "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
    }});
    if let (Some(message), Some(fields)) = (update["message"].as_object_mut(), fields.as_object()) {
        message.extend(fields.clone());
    }
    let allowlist: Allowlist = [USER].into_iter().collect();
    let (_, mut routed) = route_batch(vec![update], None, CHAT, &allowlist);
    match routed.remove(0) {
        Routed::Input(input) => Control::Message(input),
        other => panic!("not input: {other:?}"),
    }
}

fn post(session: &str, event: HookEvent) -> HookPost {
    HookPost::new(
        "box".into(),
        session.into(),
        r"C:\w\p".into(),
        String::new(),
        event,
    )
}

fn start(session: &str, source: &str, pid: u32) -> HookPost {
    post(
        session,
        HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        },
    )
}

/// Claude Code's side of one agent: the real link and channel loop.
struct Claude {
    frames: mpsc::Sender<Frame>,
    out: tokio::io::BufReader<tokio::io::DuplexStream>,
}

impl Claude {
    async fn start(addr: SocketAddr, pid: u32, work: &Path) -> Self {
        let register = Register {
            session_id: SESSION.into(),
            host: "box".into(),
            cwd: r"C:\w\p".into(),
            claude_pid: Some(pid),
            verdict_ack: true,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            client: None,
            files: true,
            session_reads: false,
        };
        let (outbox, events) = agent::spawn(LinkConfig {
            addr: addr.to_string(),
            secret: Secret::parse(SECRET).unwrap(),
            register,
            backoff: Backoff {
                initial: Duration::from_millis(20),
                max: Duration::from_millis(100),
            },
            replay: None,
        });
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let dirs = Dirs {
            project: None,
            work: Some(work.to_owned()),
        };
        tokio::spawn(agent::serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
            dirs,
            None,
            None,
        ));
        let mut claude = Self {
            frames,
            out: tokio::io::BufReader::new(theirs),
        };
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        claude
    }

    async fn send(&self, line: &str) {
        self.frames
            .send(Frame::Line(format!("{line}\n").into_bytes()))
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = Vec::new();
        tokio::time::timeout(WAIT, wire::read_line(&mut self.out, &mut line))
            .await
            .expect("a line from the agent in time")
            .expect("a whole line");
        serde_json::from_slice(&line).unwrap()
    }

    async fn send_file(&mut self, id: u32, path: &Path, caption: &str) -> Value {
        let call = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"send_file","arguments":{"path":path,"caption":caption}}});
        self.send(&call.to_string()).await;
        let answer = self.recv().await;
        assert_eq!(answer["id"], id, "{answer}");
        answer
    }
}

fn seen_methods(seen: &Seen, method: &str) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == method)
        .map(|(_, body)| body.clone())
        .collect()
}

async fn until(what: &str, done: impl Fn() -> bool) {
    let reached = async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(WAIT, reached)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
}

async fn hub_line(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> HubMsg {
    let mut line = Vec::new();
    tokio::time::timeout(WAIT, reader.read_until(b'\n', &mut line))
        .await
        .expect("a hub line in time")
        .unwrap();
    wire::decode::<HubMsg>(&line).unwrap()
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_go_both_ways_and_never_reach_the_logs() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let root = Dir(std::env::temp_dir().join(format!("cctg-files-e2e-{pid}")));
    let _ = std::fs::remove_dir_all(&root.0);
    let (state, work) = (root.0.join("state"), root.0.join("work"));
    for dir in [&state, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // Everything private carries a marker that must stay out of the logs.
    let marker = format!("zq{pid}x");
    let doc_name = format!("secret-name-{marker}.pdf");
    let doc_bytes = format!("%PDF-1.7 secret content {marker} ")
        .repeat(30_000)
        .into_bytes();
    let photo_bytes = [
        b"\xFF\xD8\xFF\xE0".to_vec(),
        format!("jpeg {marker}").into_bytes(),
    ]
    .concat();
    let files = BTreeMap::from([
        ("doc".to_owned(), doc_bytes.clone()),
        ("pic".to_owned(), photo_bytes.clone()),
    ]);

    let seen: Seen = Arc::default();
    let url = fake_telegram(files, seen.clone()).await;
    let token = Config::from_vars(|name| match name {
        "CCTG_BOT_TOKEN" => Some("1:files".to_owned()),
        "CCTG_CHAT_ID" => Some(CHAT.to_string()),
        "CCTG_ALLOWED_USER_IDS" => Some(USER.to_string()),
        _ => None,
    })
    .unwrap()
    .token;
    let api = Arc::new(BotApi::with_api_url(&url, &token, CHAT).unwrap());
    let fast = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(api.clone(), fast);
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        chat_id: CHAT,
        ..Options::default()
    };
    let mut slots = Slots::new(store.load().unwrap(), store, outbox, options);
    slots.fetch_files(api);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_agents(
        listener,
        Secret::parse(SECRET).unwrap(),
        agents,
    ));

    hooks.send(start(SESSION, "startup", 10)).await.unwrap();
    until("the topic", || {
        !seen_methods(&seen, "createForumTopic").is_empty()
    })
    .await;
    let mut claude = Claude::start(addr, 10, &work).await;
    control
        .send(topic_message(1, 100, json!({ "text": "hello" })))
        .unwrap();
    let hello = claude.recv().await;
    assert_eq!(hello["params"]["content"], "hello");

    // Topic -> session: a document, saved under the session folder.
    let caption = format!("secret caption {marker}");
    control
        .send(topic_message(
            2,
            100,
            json!({ "caption": caption, "document": {
            "file_id": "doc", "file_unique_id": "u", "file_name": doc_name,
            "mime_type": "application/pdf", "file_size": doc_bytes.len() } }),
        ))
        .unwrap();
    let note = claude.recv().await;
    let meta = &note["params"]["meta"];
    assert_eq!(
        (meta["file_kind"].as_str(), meta["message_id"].as_str()),
        (Some("document"), Some("2"))
    );
    let saved = PathBuf::from(meta["file_path"].as_str().unwrap());
    assert!(
        saved.starts_with(work.join(".cctg").join("inbox")),
        "{saved:?}"
    );
    assert!(saved.to_string_lossy().ends_with(&doc_name));
    assert_eq!(std::fs::read(&saved).unwrap(), doc_bytes);
    assert!(
        note["params"]["content"]
            .as_str()
            .unwrap()
            .ends_with(&caption)
    );
    // A photo without a name, then one Telegram says is too big.
    control
        .send(topic_message(
            3,
            100,
            json!({ "photo": [
            { "file_id": "pic", "file_unique_id": "u", "width": 10, "height": 10 } ] }),
        ))
        .unwrap();
    let note = claude.recv().await;
    let saved = PathBuf::from(note["params"]["meta"]["file_path"].as_str().unwrap());
    assert!(saved.to_string_lossy().ends_with("-photo.jpg"), "{saved:?}");
    assert_eq!(std::fs::read(&saved).unwrap(), photo_bytes);
    control
        .send(topic_message(
            4,
            100,
            json!({ "document": {
            "file_id": "huge", "file_unique_id": "u", "file_name": "big.iso" } }),
        ))
        .unwrap();
    until("the too-big notice", || {
        seen_methods(&seen, "sendMessage")
            .iter()
            .any(|body| body.contains(TOO_BIG_NOTICE))
    })
    .await;

    // Session -> topic: a picture as a photo, a text file as a document,
    // and a picture Telegram refuses as a photo still as a document.
    let out = work.join(format!("outbound-name-{marker}.png"));
    std::fs::write(
        &out,
        [b"\x89PNG\r\n\x1a\n".as_slice(), marker.as_bytes()].concat(),
    )
    .unwrap();
    let answer = claude
        .send_file(10, &out, &format!("outbound caption {marker}"))
        .await;
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let notes = work.join("notes.txt");
    std::fs::write(&notes, format!("outbound content {marker}")).unwrap();
    let answer = claude.send_file(11, &notes, "notes").await;
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let odd = work.join("odd.png");
    std::fs::write(
        &odd,
        [b"\x89PNG\r\n\x1a\n".as_slice(), b"REFUSE-PHOTO"].concat(),
    )
    .unwrap();
    let answer = claude.send_file(12, &odd, "odd").await;
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let photos = seen_methods(&seen, "sendPhoto");
    let documents = seen_methods(&seen, "sendDocument");
    assert_eq!(photos.len(), 2, "the png and the refused one");
    assert!(photos[0].contains(&format!("filename=\"outbound-name-{marker}.png\"")));
    assert!(photos[0].contains("name=\"message_thread_id\"\r\n\r\n100"));
    assert_eq!(documents.len(), 2, "the text file and the refused photo");
    assert!(documents[0].contains("filename=\"notes.txt\""));
    assert!(documents[1].contains("filename=\"odd.png\""));
    // Over 50 MB is a tool error, and nothing leaves.
    let big = work.join("big.bin");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(cctg::files::MAX_UPLOAD + 1)
        .unwrap();
    let answer = claude.send_file(13, &big, "big").await;
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    std::fs::remove_file(&big).unwrap();

    // The session ends: a photo waits as its reference and goes to the
    // agent of the resumed session.
    hooks
        .send(post(
            SESSION,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await
        .unwrap();
    drop(claude);
    tokio::time::sleep(Duration::from_millis(300)).await;
    control
        .send(topic_message(
            5,
            100,
            json!({ "caption": format!("later {marker}"), "photo": [
            { "file_id": "pic", "file_unique_id": "u", "width": 10, "height": 10 } ] }),
        ))
        .unwrap();
    let registry = state.join("registry.json");
    until("the kept file in registry.json", || {
        std::fs::read_to_string(&registry).is_ok_and(|text| text.contains("\"file_id\": \"pic\""))
    })
    .await;
    hooks.send(start(SESSION, "resume", 11)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut resumed = Claude::start(addr, 11, &work).await;
    let note = resumed.recv().await;
    assert_eq!(note["params"]["meta"]["message_id"], "5");
    assert!(
        note["params"]["content"]
            .as_str()
            .unwrap()
            .ends_with(&format!("later {marker}"))
    );
    assert_eq!(
        std::fs::read(note["params"]["meta"]["file_path"].as_str().unwrap()).unwrap(),
        photo_bytes
    );

    // An agent from before TASK-032 in a second session: the caption comes
    // as text, the topic hears why the file did not.
    hooks.send(start(OLD, "startup", 20)).await.unwrap();
    until("the second topic", || {
        seen_methods(&seen, "createForumTopic").len() == 2
    })
    .await;
    let (read, mut write) = TcpStream::connect(addr).await.unwrap().into_split();
    let mut reader = BufReader::new(read);
    let hello = wire::encode(&AgentMsg::Hello {
        secret: Secret::parse(SECRET).unwrap(),
    });
    write.write_all(&hello).await.unwrap();
    let legacy = format!(
        r#"{{"v":1,"type":"register","session_id":"{OLD}","host":"box","cwd":"C:\\w\\p","claude_pid":20}}"#
    );
    write
        .write_all(format!("{legacy}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        hub_line(&mut reader).await,
        HubMsg::Registered { files: true }
    );
    control
        .send(topic_message(
            6,
            101,
            json!({ "caption": format!("for an old agent {marker}"), "photo": [
            { "file_id": "pic", "file_unique_id": "u", "width": 10, "height": 10 } ] }),
        ))
        .unwrap();
    match hub_line(&mut reader).await {
        HubMsg::Inbound { content, .. } => {
            assert_eq!(content, format!("for an old agent {marker}"))
        }
        other => panic!("{other:?}"),
    }
    until("the old-agent notice", || {
        seen_methods(&seen, "sendMessage")
            .iter()
            .any(|body| body.contains(OLD_AGENT_NOTICE) && body.contains("101"))
    })
    .await;

    // Kinds and sizes in the logs, never names, captions or contents.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let logs = String::from_utf8_lossy(&captured.0.lock().unwrap()).into_owned();
    assert!(logs.contains("file from the topic saved"), "{logs}");
    assert!(
        logs.contains("file from the session sent to its topic"),
        "{logs}"
    );
    assert!(
        !logs.contains(&marker),
        "a private marker in the logs:\n{logs}"
    );
    for private in ["notes.txt", "odd.png", "big.iso", "inbox"] {
        assert!(!logs.contains(private), "{private} in the logs:\n{logs}");
    }
    // Nor the bot token of the download URLs (`/file/bot<token>/...`).
    assert!(!logs.contains("1:files"), "the token in the logs:\n{logs}");
}

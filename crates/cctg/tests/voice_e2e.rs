//! Voice messages to text (TASK-085) over the real pieces: the real
//! `BotApi` and `Scheduler` against a fake Telegram HTTP server (getFile,
//! the file download, sendMessage), the real `Slots` actor with its download
//! and recognition tasks, the real `serve_agents` TCP link, and the real
//! agent link and channel loop with this test as Claude Code.
//!
//! This test binary is also the `cctg-voice` stand-in: the hub starts it
//! with one argument, the model folder, whose `role.txt` says how to answer
//! (a role in the environment would not reach it: the hub scrubs `CCTG_*`).
//! The stand-in writes what came on stdin to `stdin.ogg` there, and the
//! names of the `CCTG_*` variables it sees to `env.txt`. No real model, no
//! console window, everything in a temp folder.
//!
//! Its own log subscriber: at the end, no recognized word, caption, model
//! path or token is in the logs.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::agent::{self, Backoff, Dirs, Frame, LinkConfig};
use cctg::channel::Hub;
use cctg::hub::api::BotApi;
use cctg::hub::buffer::{VOICE_FAILED_NOTICE, VOICE_HEARD_PREFIX};
use cctg::hub::chat::GroupChat;
use cctg::hub::config::{Allowlist, Config};
use cctg::hub::groups::KnownGroups;
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Scheduler};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{Routed, route_batch};
use cctg::hub::voice::Helper;
use cctg::wire::{self, HookEvent, HookPost, Register, Secret};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// The recorded voice: «привет запусти тесты и пришли логи», 4.59 s.
const VOICE: &[u8] = include_bytes!("../../voice/tests/fixtures/voice-ru.ogg");
const WORDS: &str = "привет запусти тесты и пришли логи";
const SECRET: &str = "voice-e2e-secret-0123456789";
const SESSION: &str = "701ce000-0000-4000-8000-000000000085";
const CHAT: i64 = -1000000000001;
const USER: i64 = 7_318_046_259;
const TOKEN: &str = "1:voice";
const WAIT: Duration = Duration::from_secs(30);
/// The `slow` role's sleep; its helper timeout is shorter.
const SLOW: Duration = Duration::from_secs(4);
/// Set in this process; the stand-in must not see it.
const CANARY: &str = "CCTG_VOICE_E2E_CANARY";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [model] = args.as_slice()
        && Path::new(model).join("role.txt").is_file()
    {
        stand_in(Path::new(model));
    }
    if args.iter().any(|arg| arg == "--list") {
        println!("voice_e2e: test");
        return;
    }
    // SAFETY: no other thread runs yet.
    unsafe { std::env::set_var(CANARY, "must not reach the helper") };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(scenario());
    println!("voice_e2e: ok");
}

fn stand_in(model: &Path) -> ! {
    let role = std::fs::read_to_string(model.join("role.txt")).unwrap_or_default();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    std::fs::write(model.join("stdin.ogg"), &input).unwrap();
    let seen: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.to_ascii_uppercase().starts_with("CCTG_"))
        .collect();
    std::fs::write(model.join("env.txt"), seen.join("\n")).unwrap();
    let answer = || {
        let line = json!({ "text": WORDS, "audio_ms": 4590, "took_ms": 1 });
        let mut out = std::io::stdout();
        writeln!(out, "{line}").unwrap();
        out.flush().unwrap();
    };
    match role.trim() {
        "ok" => answer(),
        "fail" => std::process::exit(1),
        "slow" => {
            std::thread::sleep(SLOW);
            std::fs::write(model.join("survived"), "").unwrap();
            answer();
        }
        other => panic!("unknown role {other:?}"),
    }
    std::process::exit(0);
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut out) = self.0.lock() {
            out.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// How often the hub logged `what` so far.
    fn count(&self, what: &str) -> usize {
        self.text().matches(what).count()
    }
}

/// What the fake Telegram saw: `(method, body)`.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Serves the voice as file `v`; answers every send with message 500.
async fn fake_telegram(seen: Seen) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let seen = seen.clone();
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
                    let (status, answer) = answer(&path, &body, &seen);
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

fn answer(path: &str, body: &[u8], seen: &Seen) -> (&'static str, Vec<u8>) {
    let ok = |result: Value| {
        (
            "200 OK",
            json!({ "ok": true, "result": result })
                .to_string()
                .into_bytes(),
        )
    };
    if path.contains("/file/bot") {
        return if path.ends_with("/voice/v.oga") {
            ("200 OK", VOICE.to_vec())
        } else {
            ("404 Not Found", b"{}".to_vec())
        };
    }
    let method = path.rsplit('/').next().unwrap_or_default().to_owned();
    let text = String::from_utf8_lossy(body).into_owned();
    seen.lock().unwrap().push((method.clone(), text));
    match method.as_str() {
        "getFile" => ok(json!({ "file_id": "v", "file_unique_id": "u",
            "file_size": VOICE.len(), "file_path": "voice/v.oga" })),
        "createForumTopic" => ok(json!({ "message_thread_id": 100, "name": "t" })),
        "sendMessage" => ok(json!({ "message_id": 500, "chat": { "id": CHAT } })),
        _ => ok(json!(true)),
    }
}

fn sends(seen: &Seen) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "sendMessage")
        .filter_map(|(_, body)| serde_json::from_str(body).ok())
        .collect()
}

/// A voice message with `caption` from the allowlisted user in topic 100,
/// through the real update parsing.
fn voice_message(message_id: i64, caption: &str) -> Control {
    let update = json!({ "update_id": message_id, "message": {
        "message_id": message_id, "message_thread_id": 100, "is_topic_message": true,
        "date": 1, "from": { "id": USER, "is_bot": false, "first_name": "x" },
        "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
        "caption": caption,
        "voice": { "file_id": "v", "file_unique_id": "u", "duration": 5,
            "mime_type": "audio/ogg", "file_size": VOICE.len() },
    }});
    let allowlist: Allowlist = [USER].into_iter().collect();
    let groups = KnownGroups::of([GroupChat::of(CHAT)]);
    let (_, mut routed) = route_batch(vec![update], None, &groups, &allowlist);
    match routed.remove(0) {
        Routed::Input(input) => Control::Message(input),
        other => panic!("not input: {other:?}"),
    }
}

fn start(pid: u32) -> HookPost {
    HookPost::new(
        "box".into(),
        SESSION.into(),
        r"C:\w\p".into(),
        String::new(),
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        },
    )
}

/// Claude Code's side of the agent: the real link and channel loop.
struct Claude {
    frames: mpsc::Sender<Frame>,
    out: tokio::io::BufReader<tokio::io::DuplexStream>,
}

impl Claude {
    async fn start(addr: SocketAddr, work: &Path) -> Self {
        let register = Register {
            session_id: SESSION.into(),
            host: "box".into(),
            cwd: r"C:\w\p".into(),
            claude_pid: Some(10),
            verdict_ack: true,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            console_line_chars: 0,
            client: None,
            files: true,
            session_reads: false,
            status_lines: false,
            private_place: false,
            enrolled: None,
            heartbeat: false,
        };
        let (outbox, events) = agent::spawn(LinkConfig {
            addr: cctg::tls::HubAddr::plain(addr.to_string()),
            secret: Secret::parse(SECRET).unwrap(),
            register,
            backoff: Backoff {
                initial: Duration::from_millis(20),
                max: Duration::from_millis(100),
            },
            replay: None,
            heartbeat: Default::default(),
            status: None,
        });
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let dirs = Dirs {
            project: None,
            work: Some(work.to_owned()),
            claude: None,
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

/// One hub whose helper answers as `role` within `timeout`, a live session
/// with its agent in topic 100: what the fake Telegram saw, the session's
/// Claude Code, the helper's model folder and the session's folder.
struct Rig {
    control: mpsc::UnboundedSender<Control>,
    _hooks: mpsc::Sender<HookPost>,
    seen: Seen,
    claude: Claude,
    model: PathBuf,
    work: PathBuf,
}

async fn rig(root: &Path, role: &str, timeout: Duration, captured: &Captured) -> Rig {
    let base = root.join(role);
    let (state, work, model) = (base.join("state"), base.join("work"), base.join("model"));
    for dir in [&state, &work, &model] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(model.join("role.txt"), role).unwrap();
    let seen: Seen = Arc::default();
    let url = fake_telegram(seen.clone()).await;
    let token = Config::from_vars(|name| match name {
        "CCTG_BOT_TOKEN" => Some(TOKEN.to_owned()),
        "CCTG_CHAT_ID" => Some(CHAT.to_string()),
        "CCTG_ALLOWED_USER_IDS" => Some(USER.to_string()),
        _ => None,
    })
    .unwrap()
    .token;
    let api = Arc::new(BotApi::with_api_url(&url, &token, None).unwrap());
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
        ..Options::default()
    };
    let mut slots = Slots::new(
        store.load(GroupChat::of(CHAT)).unwrap(),
        store,
        outbox,
        options,
    );
    slots.fetch_files(api.clone());
    slots.recognize_voices(
        api,
        Arc::new(Helper {
            program: std::env::current_exe().unwrap(),
            model: model.clone(),
            timeout,
        }),
    );
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
    let topics = captured.count("forum topic created");
    hooks.send(start(10)).await.unwrap();
    until("the topic", || {
        captured.count("forum topic created") > topics
    })
    .await;
    let bound = captured.count("agent bound to its session");
    let claude = Claude::start(addr, &work).await;
    until("the agent bound", || {
        captured.count("agent bound to its session") > bound
    })
    .await;
    Rig {
        control,
        _hooks: hooks,
        seen,
        claude,
        model,
        work,
    }
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn scenario() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");
    let pid = std::process::id();
    let root = Dir(std::env::temp_dir().join(format!("cctg-voice-e2e-{pid}")));
    let _ = std::fs::remove_dir_all(&root.0);
    // A caption carries a marker that must stay out of the logs.
    let marker = format!("zq{pid}x");
    let caption = format!("voice caption {marker}");

    // Recognized: the words are posted as a reply to the voice before the
    // session hears of it; the session gets the words and the file.
    let mut ok = rig(&root.0, "ok", WAIT, &captured).await;
    ok.control.send(voice_message(7, &caption)).unwrap();
    let note = ok.claude.recv().await;
    let posted = sends(&ok.seen);
    let post = posted
        .iter()
        .find(|body| body["text"] == format!("{VOICE_HEARD_PREFIX}{WORDS}"))
        .unwrap_or_else(|| panic!("the words posted before the session heard: {posted:?}"));
    assert_eq!(post["reply_parameters"]["message_id"], 7, "{post}");
    assert_eq!(post["message_thread_id"], 100, "{post}");
    // The agent says where it saved the file, then the words.
    let content = note["params"]["content"].as_str().unwrap();
    assert!(
        content.ends_with(&format!("\n\n(голосовое, распознано) {WORDS}\n{caption}")),
        "{note}"
    );
    let meta = &note["params"]["meta"];
    assert_eq!(
        (meta["file_kind"].as_str(), meta["message_id"].as_str()),
        (Some("voice"), Some("7")),
        "{note}"
    );
    let saved = PathBuf::from(meta["file_path"].as_str().unwrap());
    assert!(saved.starts_with(&ok.work), "{saved:?}");
    assert_eq!(std::fs::read(&saved).unwrap(), VOICE);
    assert_eq!(std::fs::read(ok.model.join("stdin.ogg")).unwrap(), VOICE);
    assert_eq!(
        std::fs::read_to_string(ok.model.join("env.txt")).unwrap(),
        "",
        "the helper saw CCTG_ variables"
    );

    // A helper that fails: the author is told, the file still goes.
    let mut fail = rig(&root.0, "fail", WAIT, &captured).await;
    fail.control.send(voice_message(8, &caption)).unwrap();
    let note = fail.claude.recv().await;
    assert!(
        note["params"]["content"]
            .as_str()
            .unwrap()
            .ends_with(&format!("\n\n(голосовое, не распознано)\n{caption}")),
        "{note}"
    );
    assert_eq!(
        std::fs::read(note["params"]["meta"]["file_path"].as_str().unwrap()).unwrap(),
        VOICE
    );
    until("the failure notice", || {
        sends(&fail.seen)
            .iter()
            .any(|body| body["text"] == VOICE_FAILED_NOTICE)
    })
    .await;
    assert!(
        !sends(&fail.seen).iter().any(|body| body["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(VOICE_HEARD_PREFIX))),
        "nothing heard, nothing posted"
    );

    // Too slow: killed at the timeout, it never finishes; the voice goes on.
    let mut slow = rig(&root.0, "slow", Duration::from_secs(1), &captured).await;
    let started = Instant::now();
    slow.control.send(voice_message(9, &caption)).unwrap();
    let note = slow.claude.recv().await;
    assert!(
        note["params"]["content"]
            .as_str()
            .unwrap()
            .ends_with(&format!("\n\n(голосовое, не распознано)\n{caption}")),
        "{note}"
    );
    assert!(started.elapsed() < SLOW, "{:?}", started.elapsed());
    until("the failure notice", || {
        sends(&slow.seen)
            .iter()
            .any(|body| body["text"] == VOICE_FAILED_NOTICE)
    })
    .await;
    tokio::time::sleep((SLOW + Duration::from_secs(1)).saturating_sub(started.elapsed())).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !slow.model.join("survived").exists(),
        "the slow helper was not killed"
    );

    // Outcomes and times in the logs, never words, captions, paths or the
    // token.
    let logs = captured.text();
    assert!(logs.contains("voice recognition"), "{logs}");
    for outcome in [
        "outcome=\"ok\"",
        "outcome=\"failed\"",
        "outcome=\"timeout\"",
    ] {
        assert!(logs.contains(outcome), "{outcome} not logged:\n{logs}");
    }
    assert!(!logs.contains(&marker), "the caption in the logs:\n{logs}");
    assert!(!logs.contains("запусти"), "the words in the logs:\n{logs}");
    assert!(
        !logs.contains("распознано"),
        "the words in the logs:\n{logs}"
    );
    assert!(
        !logs.contains(&format!("cctg-voice-e2e-{pid}")),
        "a path in the logs:\n{logs}"
    );
    assert!(!logs.contains(TOKEN), "the token in the logs:\n{logs}");
}

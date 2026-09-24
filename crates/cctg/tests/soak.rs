//! Multi-slot routing soak (TASK-018), step 4 of the development plan.
//!
//! Five runs on one device: two concurrent top-level sessions in folder A, one
//! in folder B, a nested `claude -p` inside the first A session, and a fifth A
//! session that starts while the hub is down, after the first one ended. Three
//! topics are expected; the fifth session takes the freed `[host] A` slot with
//! one separator.
//!
//! Everything but Telegram and Claude Code is real: `cctg hook` and `cctg
//! agent` processes (home, state and `CLAUDE_CONFIG_DIR` in a temp dir), the
//! hub's `serve_hooks`, `serve_agents`, `Slots`, `Scheduler` and `updates::poll`.
//! Each session is a stand-in process named `claude(.exe)` (a copy of this
//! test binary) that runs its hooks and its agent as children, with the env
//! Claude Code sets (`CLAUDE_PID`, `CLAUDE_CODE_SESSION_ID`, ...). A launcher
//! starts it and exits, so the process tree above a top-level stand-in ends
//! there (as under a terminal) even when this test runs inside Claude Code;
//! the nested run's stand-in is a child of the first A stand-in, so the hook's
//! own process-tree walk (TASK-012) finds its parent.
//!
//! Telegram is a fake by default: it numbers topics and messages, answers
//! `editForumTopic` with a `forum_topic_edited` service update that reaches the
//! hub through the real `updates::poll`, and answers 429 on chosen stream
//! sends. `CCTG_SOAK_LIVE=1` runs the same scenario against the real bot
//! (`CCTG_SOAK_ENV`, default `<repo>/.env`; the hub must not run meanwhile).
//! There it touches only its own three topics, keeps ops on simulated
//! message ids local and deletes the topics at the end, also after a failed
//! check: see `docs/soak.md`.
//!
//! Slow (about a minute), so it runs only when asked:
//! `cargo test -p cctg --test soak -- --ignored`.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::device::canonical_cwd;
use cctg::hub::api::{ApiError, BotApi, ForumTopic, Message};
use cctg::hub::config::{Allowlist, BotToken, Config};
use cctg::hub::ingress::{serve_agents, serve_hooks};
use cctg::hub::offset::OffsetStore;
use cctg::hub::registry::{
    Icons, RegistryStore, folder_name, nested_header, separator, topic_title,
};
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{self, Inbound, Routed, ServiceKind, UpdateSource};
use cctg::wire::Secret;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod common;

const SECRET: &str = "soak-secret-0123456789abcdef";
const HOST: &str = "soakbox";
const FAKE_CHAT: i64 = -1000000000001;
const FAKE_USER: i64 = 1001;
const BOT: i64 = 3003;
const ROLE: &str = "CCTG_SOAK_ROLE";
/// Ids of simulated user messages against the real chat start here; they
/// name no real message, so the ops that act on them stay local.
const SYNTHETIC_MESSAGE: i64 = 2_000_000_000;
/// Callback query ids of simulated button presses start with this.
const SYNTHETIC_QUERY: &str = "soak-";
/// Waits are this many times longer against the real bot (20 messages a
/// minute instead of the fake's fast bucket).
static SLOW: AtomicU64 = AtomicU64::new(1);

const A1: &str = "a1a1a1a1-0000-4000-8000-000000000001";
const A2: &str = "a2a2a2a2-0000-4000-8000-000000000002";
const B1: &str = "b1b1b1b1-0000-4000-8000-000000000003";
const NESTED: &str = "ee0e0e0e-0000-4000-8000-000000000004";
const A5: &str = "a5a5a5a5-0000-4000-8000-000000000005";

fn short(session: &str) -> &str {
    &session[..8]
}

fn main() {
    match std::env::var(ROLE).as_deref() {
        Ok("launch") => return launch(),
        Ok("claude") => return stand_in(),
        _ => {}
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        return;
    }
    if !args
        .iter()
        .any(|arg| arg == "--ignored" || arg == "--include-ignored")
    {
        println!("soak: skipped (slow); run `cargo test -p cctg --test soak -- --ignored`");
        return;
    }
    let live = std::env::var("CCTG_SOAK_LIVE").is_ok_and(|value| value == "1");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    let report = runtime.block_on(soak(live));
    println!("{report}");
    if let Ok(path) = std::env::var("CCTG_SOAK_REPORT") {
        std::fs::write(path, &report).expect("write the report");
    }
    println!("soak: ok");
}

// ------------------------------------------------------------ stand-in side

/// The launcher: starts the stand-in with this process's stdio and exits at
/// once, so the stand-in's parent is gone before any hook walks the tree.
#[expect(
    clippy::zombie_processes,
    reason = "the launcher exits at once; the harness talks to the stand-in"
)]
fn launch() {
    Command::new(std::env::current_exe().expect("own path"))
        .env(ROLE, "claude")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the stand-in");
}

fn out(session: &str, mut line: Value) {
    line["session"] = session.into();
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "@@{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// A Claude Code stand-in: commands on stdin (one JSON object per line),
/// results and the agent's JSON-RPC output on stdout as `@@<json>` lines.
/// With `SOAK_SCRIPT` it runs those hooks and exits (a nested `claude -p`).
fn stand_in() {
    let session = env("SOAK_SESSION");
    let cctg = env("SOAK_CCTG");
    let prepare = |command: &mut Command| {
        command
            .env("CLAUDE_PID", std::process::id().to_string())
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env("CLAUDE_CODE_ENTRYPOINT", env("SOAK_ENTRYPOINT"))
            .env("CLAUDECODE", "1")
            .env_remove(ROLE)
            .env_remove("SOAK_SCRIPT")
            .current_dir(env("SOAK_CWD"));
    };
    let hook = |event: &str, input: &Value| {
        let started = Instant::now();
        let mut command = Command::new(&cctg);
        prepare(&mut command);
        let child = command
            .args(["hook", event])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            return json!({"hook": event, "error": "spawn"});
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.to_string().as_bytes());
        }
        let output = child.wait_with_output();
        let (stdout, stderr) = output
            .map(|output| {
                (
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )
            })
            .unwrap_or_default();
        json!({
            "hook": event,
            "ms": started.elapsed().as_millis() as u64,
            "stdout": stdout,
            "stderr": stderr,
        })
    };
    if let Ok(script) = std::env::var("SOAK_SCRIPT") {
        let items: Vec<Value> = serde_json::from_str(&script).unwrap_or_default();
        for item in items {
            if let Some(ms) = item["sleep_ms"].as_u64() {
                std::thread::sleep(Duration::from_millis(ms));
            } else if let Some(event) = item["hook"].as_str() {
                out(&session, hook(event, &item["input"]));
            }
        }
        return;
    }
    out(&session, json!({"ready": std::process::id()}));
    let mut agent: Option<(Child, ChildStdin)> = None;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(command) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(event) = command["hook"].as_str() {
            out(&session, hook(event, &command["input"]));
        } else if command.get("agent").is_some() {
            let mut spawn = Command::new(&cctg);
            prepare(&mut spawn);
            let mut child = spawn
                .arg("agent")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("start cctg agent");
            let stdin = child.stdin.take().expect("agent stdin");
            let stdout = child.stdout.take().expect("agent stdout");
            let session = session.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(rpc) = serde_json::from_str::<Value>(&line) {
                        out(&session, json!({"agent": rpc}));
                    }
                }
            });
            agent = Some((child, stdin));
        } else if let Some(rpc) = command.get("rpc") {
            if let Some((_, stdin)) = agent.as_mut() {
                let _ = writeln!(stdin, "{rpc}");
                let _ = stdin.flush();
            }
        } else if let Some(nest) = command.get("nest") {
            let nested = nest["session"].as_str().unwrap_or_default().to_owned();
            let mut child = Command::new(std::env::current_exe().expect("own path"))
                .env(ROLE, "claude")
                .env("SOAK_SESSION", &nested)
                .env("SOAK_ENTRYPOINT", "sdk-cli")
                .env("SOAK_SCRIPT", nest["script"].to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the nested stand-in");
            let stdout = child.stdout.take().expect("nested stdout");
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
            let _ = child.wait();
            out(&session, json!({"nest_done": nested}));
        } else if command.get("exit").is_some() {
            break;
        }
    }
    // Claude Code closes the agent's stdin when the session ends.
    if let Some((mut child, stdin)) = agent {
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

// ------------------------------------------------------------ harness side

/// One simulated session: its folder, transcript and stand-in.
struct Sim {
    id: &'static str,
    cwd: String,
    transcript: PathBuf,
    stdin: Option<ChildStdin>,
    pid: u32,
    /// Every `@@` line of this stand-in (the nested run's too).
    lines: Arc<Mutex<Vec<Value>>>,
}

impl Sim {
    fn send(&mut self, command: Value) {
        let stdin = self.stdin.as_mut().expect("stand-in running");
        writeln!(stdin, "{command}").expect("stand-in stdin");
        stdin.flush().expect("stand-in stdin");
    }

    fn lines_of(&self, session: &str) -> Vec<Value> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line["session"] == session)
            .cloned()
            .collect()
    }

    fn hooks_done(&self, session: &str) -> Vec<Value> {
        self.lines_of(session)
            .into_iter()
            .filter(|line| line.get("hook").is_some())
            .collect()
    }

    fn input(&self, event: &str, extra: Value) -> Value {
        let mut input = json!({
            "session_id": self.id,
            "cwd": self.cwd,
            "transcript_path": self.transcript.to_string_lossy(),
            "hook_event_name": event,
        });
        if let (Some(input), Some(extra)) = (input.as_object_mut(), extra.as_object()) {
            input.extend(extra.clone());
        }
        input
    }

    /// Runs one hook in the stand-in and waits for it; returns its result.
    async fn hook(&mut self, event: &str, extra: Value) -> Value {
        let before = self.hooks_done(self.id).len();
        let input = self.input(event, extra);
        self.send(json!({"hook": event, "input": input}));
        let id = self.id;
        wait_for(&format!("{event} hook of {}", short(id)), 20, || {
            self.hooks_done(id).len() > before
        })
        .await;
        let done = self.hooks_done(id).pop().expect("hook result");
        assert_eq!(done["stdout"], "", "a hook never writes stdout");
        assert!(
            !done["stderr"].as_str().unwrap_or_default().contains(SECRET),
            "{done}"
        );
        done
    }

    fn start_agent(&mut self) {
        self.send(json!({"agent": true}));
        self.rpc(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25"}}));
        self.rpc(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn rpc(&mut self, rpc: Value) {
        self.send(json!({"rpc": rpc}));
    }

    /// JSON-RPC lines the session's agent wrote.
    fn agent_out(&self) -> Vec<Value> {
        self.lines_of(self.id)
            .into_iter()
            .filter_map(|line| line.get("agent").cloned())
            .collect()
    }

    fn inbound(&self) -> Vec<String> {
        self.agent_out()
            .into_iter()
            .filter(|rpc| rpc["method"] == "notifications/claude/channel")
            .map(|rpc| {
                rpc["params"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    fn verdicts(&self) -> Vec<(String, String)> {
        self.agent_out()
            .into_iter()
            .filter(|rpc| rpc["method"] == "notifications/claude/channel/permission")
            .map(|rpc| {
                (
                    rpc["params"]["request_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    rpc["params"]["behavior"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect()
    }

    fn append(&self, text: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .expect("transcript");
        file.write_all(text.as_bytes()).expect("transcript write");
    }

    /// The session ends: the stand-in closes its agent and exits.
    fn close(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, "{}", json!({"exit": true}));
        }
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.close();
    }
}

fn prompt_line(text: &str) -> String {
    format!(
        "{}\n",
        json!({"type":"user","isMeta":false,"message":{"role":"user","content":text}})
    )
}

fn tool_lines(id: &str, description: &str) -> String {
    let call = json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use",
        "content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":"true","description":description}}]}});
    let result = json!({"type":"user","message":{"role":"user",
        "content":[{"type":"tool_result","tool_use_id":id,"content":"ok","is_error":false}]}});
    format!("{call}\n{result}\n")
}

async fn wait_for(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs * SLOW.load(Ordering::Relaxed));
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ------------------------------------------------------------ Telegram

#[derive(Debug, Clone)]
struct Call {
    at: Instant,
    run: usize,
    kind: &'static str,
    thread: Option<i64>,
    text: String,
    /// The message id Telegram gave (sends), or the one acted on.
    message_id: Option<i64>,
    outcome: &'static str,
}

fn describe(op: &Op) -> (&'static str, Option<i64>, String, Option<i64>) {
    match op {
        Op::Send {
            thread_id,
            text,
            permission,
            ..
        } => (
            if *permission { "permission" } else { "send" },
            *thread_id,
            text.clone(),
            None,
        ),
        Op::SendDocument {
            thread_id,
            document,
            ..
        } => ("document", *thread_id, document.file_name.clone(), None),
        Op::Stream {
            thread_id, text, ..
        } => ("stream", Some(*thread_id), text.clone(), None),
        Op::Edit {
            message_id, text, ..
        } => ("edit", None, text.clone(), Some(*message_id)),
        Op::React { message_id, emoji } => ("react", None, emoji.clone(), Some(*message_id)),
        Op::AnswerCallback { query_id, .. } => ("callback", None, query_id.clone(), None),
        Op::Delete { message_id } => ("delete", None, String::new(), Some(*message_id)),
        Op::Pin { message_id } => ("pin", None, String::new(), Some(*message_id)),
        Op::CreateTopic { name, .. } => ("create_topic", None, name.clone(), None),
        Op::EditTopic {
            thread_id,
            name,
            icon_custom_emoji_id,
        } => (
            "edit_topic",
            Some(*thread_id),
            format!(
                "{}|{}",
                name.clone().unwrap_or_default(),
                icon_custom_emoji_id.clone().unwrap_or_default()
            ),
            None,
        ),
    }
}

/// The Telegram side: a fake, or the real bot with every call recorded.
struct Tg {
    live: Option<BotApi>,
    calls: Mutex<Vec<Call>>,
    run: AtomicUsize,
    next_topic: AtomicI64,
    next_message: AtomicI64,
    /// Stream sends still to go before each planned 429.
    flood: Mutex<VecDeque<usize>>,
    streams_seen: AtomicUsize,
    retry_after: Duration,
    updates: Updates,
    /// `forum_topic_edited` service messages Telegram showed: (thread, id).
    service: Mutex<Vec<(Option<i64>, i64)>>,
}

impl Tg {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self, kind: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.kind == kind && call.outcome == "ok")
            .count()
    }

    /// Accepted message texts (sends, prompts, stream lines) of a topic.
    fn texts(&self, thread: i64) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| {
                call.thread == Some(thread)
                    && call.outcome == "ok"
                    && matches!(call.kind, "send" | "permission" | "stream")
            })
            .flat_map(|call| call.text.lines().map(str::to_owned).collect::<Vec<_>>())
            .collect()
    }

    fn arm_flood(&self, after_streams: &[usize]) {
        let seen = self.streams_seen.load(Ordering::SeqCst);
        self.flood
            .lock()
            .unwrap()
            .extend(after_streams.iter().map(|n| seen + n));
    }

    fn fake_execute(&self, op: &Op) -> Delivery {
        let sent = |id| {
            Ok(Outcome::Sent(Message {
                message_id: id,
                ..Message::default()
            }))
        };
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: self.next_topic.fetch_add(1, Ordering::SeqCst),
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Stream { .. } => {
                let seen = self.streams_seen.fetch_add(1, Ordering::SeqCst) + 1;
                let mut flood = self.flood.lock().unwrap();
                if flood.front() == Some(&seen) {
                    flood.pop_front();
                    return Err(ApiError::RetryAfter(self.retry_after));
                }
                sent(self.next_message.fetch_add(1, Ordering::SeqCst))
            }
            Op::Send { .. } | Op::SendDocument { .. } => {
                sent(self.next_message.fetch_add(1, Ordering::SeqCst))
            }
            Op::EditTopic { thread_id, .. } => {
                // Telegram posts a service message into the topic.
                let id = self.next_message.fetch_add(1, Ordering::SeqCst);
                self.service.lock().unwrap().push((Some(*thread_id), id));
                self.updates.push(json!({"message": {
                    "message_id": id, "message_thread_id": thread_id, "is_topic_message": true,
                    "date": 1, "chat": {"id": FAKE_CHAT, "type": "supergroup", "is_forum": true},
                    "from": {"id": BOT, "is_bot": true, "first_name": "bot"},
                    "forum_topic_edited": {"name": "x"},
                }}));
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

/// Ops on a simulated user message or button press. Against the real chat
/// they are answered here and never sent: their ids name nothing real.
fn synthetic(op: &Op) -> bool {
    match op {
        Op::React { message_id, .. } => *message_id >= SYNTHETIC_MESSAGE,
        Op::AnswerCallback { query_id, .. } => query_id.starts_with(SYNTHETIC_QUERY),
        _ => false,
    }
}

impl Transport for Tg {
    async fn execute(&self, op: &Op) -> Delivery {
        let at = Instant::now();
        let local = self.live.is_some() && synthetic(op);
        let result = match &self.live {
            Some(_) if local => Ok(Outcome::Done),
            Some(api) => api.execute(op).await,
            None => self.fake_execute(op),
        };
        let (kind, thread, text, acted_on) = describe(op);
        let message_id = match &result {
            Ok(Outcome::Sent(message)) => Some(message.message_id),
            Ok(Outcome::Topic(topic)) => Some(topic.message_thread_id),
            _ => acted_on,
        };
        let outcome = match &result {
            Ok(_) if local => "synthetic",
            Ok(_) => "ok",
            Err(ApiError::RetryAfter(_)) => "429",
            Err(_) => "error",
        };
        self.calls.lock().unwrap().push(Call {
            at,
            run: self.run.load(Ordering::SeqCst),
            kind,
            thread,
            text,
            message_id,
            outcome,
        });
        result
    }
}

/// Updates the fake Telegram has for `getUpdates`.
#[derive(Clone)]
struct Updates {
    tx: mpsc::UnboundedSender<Value>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
    next: Arc<AtomicI64>,
}

impl Updates {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            next: Arc::new(AtomicI64::new(1)),
        }
    }

    fn push(&self, mut update: Value) {
        update["update_id"] = self.next.fetch_add(1, Ordering::SeqCst).into();
        let _ = self.tx.send(update);
    }
}

impl UpdateSource for Updates {
    fn chat_id(&self) -> i64 {
        FAKE_CHAT
    }

    async fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Value>, ApiError> {
        let mut rx = self.rx.lock().await;
        let Some(first) = rx.recv().await else {
            return std::future::pending().await;
        };
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        Ok(batch)
    }
}

// ------------------------------------------------------------ hub

struct Soak {
    live: bool,
    /// Live only: for deleting this run's topics at the end.
    token: Option<BotToken>,
    root: PathBuf,
    home: PathBuf,
    config_dir: PathBuf,
    state: PathBuf,
    bin: PathBuf,
    agent_port: u16,
    hook_port: u16,
    tg: Arc<Tg>,
    bucket: BucketConfig,
    options: Options,
    allowlist: Allowlist,
    next_message: AtomicI64,
}

struct Hub {
    control: mpsc::UnboundedSender<Control>,
    tasks: Vec<JoinHandle<()>>,
}

/// Stopping the hub (also when a failed assertion unwinds the scenario)
/// aborts its tasks, so nothing of it outlives the run.
impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn bind(port: u16) -> TcpListener {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return listener,
            Err(error) => {
                assert!(Instant::now() < deadline, "rebind {port}: {error}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

impl Soak {
    async fn start_hub(&self, run: usize) -> Hub {
        self.tg.run.store(run, Ordering::SeqCst);
        let agents_listener = bind(self.agent_port).await;
        let hooks_listener = bind(self.hook_port).await;
        let (scheduler, outbox) = Scheduler::new(self.tg.clone(), self.bucket);
        let store = RegistryStore::open(&self.state).expect("registry store");
        let registry = store.load().expect("registry loads");
        let (slots, _view) = Slots::new(registry, store, outbox, self.options.clone());
        let (agents, agents_rx) = mpsc::channel(256);
        let (hooks, hooks_rx) = mpsc::channel(256);
        let (control, control_rx) = mpsc::unbounded_channel();
        let secret = Secret::parse(SECRET).expect("secret");
        let mut tasks = vec![
            tokio::spawn(scheduler.run()),
            tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)),
            tokio::spawn(serve_agents(agents_listener, secret.clone(), agents)),
            tokio::spawn(serve_hooks(hooks_listener, secret, hooks)),
        ];
        let offsets = OffsetStore::open(&self.state).expect("offset store");
        let (allowlist, tg, router) = (self.allowlist.clone(), self.tg.clone(), control.clone());
        let live = self.live;
        let route = move |routed: Routed| match routed {
            Routed::Service(service) if service.kind == ServiceKind::TopicEdited => {
                if live {
                    tg.service
                        .lock()
                        .unwrap()
                        .push((service.thread_id, service.message_id));
                }
                let _ = router.send(Control::TopicEdited {
                    thread_id: service.thread_id,
                    message_id: service.message_id,
                });
            }
            // Live: only service messages; the harness speaks for the user.
            Routed::Input(input) if !live => {
                let _ = router.send(Control::Message(input));
            }
            Routed::Callback(input) if !live => {
                let _ = router.send(Control::Callback(input));
            }
            _ => {}
        };
        tasks.push(match &self.tg.live {
            Some(_) => {
                let tg = self.tg.clone();
                tokio::spawn(async move {
                    let api = tg.live.as_ref().expect("live api");
                    updates::poll(api, &allowlist, &offsets, route).await;
                })
            }
            None => {
                let source = self.tg.updates.clone();
                tokio::spawn(async move {
                    updates::poll(&source, &allowlist, &offsets, route).await;
                })
            }
        });
        Hub { control, tasks }
    }

    /// A user message in a topic.
    fn say(&self, hub: &Hub, thread: i64, text: &str) {
        let message_id = self.next_message.fetch_add(1, Ordering::SeqCst);
        if self.live {
            let _ = hub.control.send(Control::Message(Inbound {
                message_id,
                thread_id: Some(thread),
                text: Some(text.to_owned()),
                reply_to: None,
                quote: None,
                forwarded: false,
            }));
            return;
        }
        let chat = json!({"id": FAKE_CHAT, "type": "supergroup", "is_forum": true});
        self.tg.updates.push(json!({"message": {
            "message_id": message_id, "message_thread_id": thread, "is_topic_message": true,
            "date": 1, "chat": chat, "text": text,
            "from": {"id": FAKE_USER, "is_bot": false, "first_name": "u"},
            "reply_to_message": {"message_id": thread, "date": 1, "chat": chat},
        }}));
    }

    /// A press on a permission button.
    fn press(&self, hub: &Hub, message_id: i64, data: &str) {
        let query = format!("{SYNTHETIC_QUERY}{message_id}");
        if self.live {
            let _ = hub.control.send(Control::Callback(updates::CallbackInput {
                query_id: query,
                data: Some(data.to_owned()),
                message_id: Some(message_id),
            }));
            return;
        }
        let chat = json!({"id": FAKE_CHAT, "type": "supergroup", "is_forum": true});
        self.tg.updates.push(json!({"callback_query": {
            "id": query, "data": data,
            "from": {"id": FAKE_USER, "is_bot": false, "first_name": "u"},
            "message": {"message_id": message_id, "date": 1, "chat": chat},
        }}));
    }

    /// Starts a session's stand-in in `folder` (through the launcher, so its
    /// parent is gone) and waits until it runs.
    async fn launch(&self, id: &'static str, folder: &Path) -> Sim {
        let project = self.config_dir.join("projects").join(format!(
            "C--soak-{}",
            folder.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let transcript = project.join(format!("{id}.jsonl"));
        std::fs::write(&transcript, "").unwrap();
        let mut command = Command::new(&self.bin);
        common::isolate(&mut command, &self.home);
        let mut launcher = command
            .env(ROLE, "launch")
            .env("SOAK_SESSION", id)
            .env("SOAK_ENTRYPOINT", "cli")
            .env("SOAK_CCTG", env!("CARGO_BIN_EXE_cctg"))
            .env("SOAK_CWD", folder)
            .env("CLAUDE_CONFIG_DIR", &self.config_dir)
            .env_remove("SOAK_SCRIPT")
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the launcher");
        let stdin = launcher.stdin.take().expect("launcher stdin");
        let stdout = launcher.stdout.take().expect("launcher stdout");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(value) = line
                    .strip_prefix("@@")
                    .and_then(|rest| serde_json::from_str::<Value>(rest).ok())
                {
                    sink.lock().unwrap().push(value);
                }
            }
        });
        assert!(launcher.wait().expect("launcher exits").success());
        let mut sim = Sim {
            id,
            cwd: canonical_cwd(&folder.to_string_lossy()),
            transcript,
            stdin: Some(stdin),
            pid: 0,
            lines,
        };
        wait_for(&format!("stand-in {}", short(id)), 20, || {
            sim.lines_of(id)
                .iter()
                .any(|line| line.get("ready").is_some())
        })
        .await;
        sim.pid = sim
            .lines_of(id)
            .iter()
            .find_map(|line| line["ready"].as_u64())
            .expect("pid") as u32;
        sim
    }

    fn registry(&self) -> Value {
        let text = std::fs::read_to_string(self.state.join("registry.json")).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    fn spool_files(&self, session: &str) -> usize {
        std::fs::read_dir(self.home.join(".cctg").join("spool").join(session))
            .map(|dir| dir.count())
            .unwrap_or(0)
    }
}

/// Waits until Telegram has been quiet for `quiet`.
async fn settle(tg: &Tg, quiet: Duration, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs * SLOW.load(Ordering::Relaxed));
    let quiet = quiet * SLOW.load(Ordering::Relaxed) as u32;
    loop {
        let last = tg.calls().last().map(|call| call.at);
        if last.is_none_or(|at| at.elapsed() >= quiet) {
            return;
        }
        assert!(Instant::now() < deadline, "Telegram never went quiet");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn topic_of(tg: &Tg, name: &str) -> Option<i64> {
    tg.calls()
        .iter()
        .find(|call| call.kind == "create_topic" && call.outcome == "ok" && call.text == name)
        .and_then(|call| call.message_id)
}

/// The icon last set on `thread` in hub run `run`.
fn last_icon(tg: &Tg, thread: i64, run: usize) -> Option<String> {
    tg.calls()
        .iter()
        .rev()
        .filter(|call| call.kind == "edit_topic" && call.outcome == "ok" && call.run == run)
        .filter(|call| call.thread == Some(thread))
        .find_map(|call| {
            let (_, icon) = call.text.split_once('|')?;
            (!icon.is_empty()).then(|| icon.to_owned())
        })
}

// ------------------------------------------------------------ scenario

async fn soak(live: bool) -> String {
    let started = Instant::now();
    let root = std::env::temp_dir().join(format!("cctg-soak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Removes the folder also when the preparation below fails.
    let _root_guard = RemoveOnDrop(root.clone());
    let (home, config_dir, state, work) = (
        root.join("home"),
        root.join("cfg"),
        root.join("state"),
        root.join("work"),
    );
    for dir in [
        &home.join(".cctg"),
        &config_dir,
        &state,
        &work.join("A"),
        &work.join("B"),
        &root.join("bin"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // The stand-in: this binary under Claude Code's process name.
    let bin = root.join("bin").join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::copy(std::env::current_exe().unwrap(), &bin).expect("copy the stand-in");
    let (agent_port, hook_port) = {
        let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let h = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        (
            a.local_addr().unwrap().port(),
            h.local_addr().unwrap().port(),
        )
    };
    std::fs::write(
        home.join(".cctg").join("device.env"),
        format!(
            "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR=127.0.0.1:{hook_port}\n\
             CCTG_HUB_AGENT_ADDR=127.0.0.1:{agent_port}\nCCTG_HOST={HOST}\n"
        ),
    )
    .unwrap();

    if live {
        SLOW.store(10, Ordering::Relaxed);
    }
    let updates = Updates::new();
    let mut foreign_pending = None;
    let (live_api, token, options, bucket, allowlist) = if live {
        let env_file = std::env::var("CCTG_SOAK_ENV")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
        let config = Config::load(Some(&env_file)).expect("live config (values not shown)");
        let api = BotApi::new(&config.token, config.chat_id).expect("bot api");
        // The run creates topics, deletes their service messages and at the
        // end the topics themselves.
        let me = api.get_me().await.expect("getMe");
        let member = api.get_chat_member(me.id).await.expect("getChatMember");
        assert!(
            member.status == "creator" || (member.can_manage_topics && member.can_delete_messages),
            "the bot needs can_manage_topics and can_delete_messages"
        );
        // Updates still pending for the bot (written while the hub was
        // stopped) are read and confirmed by this run's first poll, so the
        // hub never sees them. A read without an offset confirms nothing;
        // only the count is shown (Telegram returns at most 100).
        let pending = api
            .get_updates(None, Duration::ZERO)
            .await
            .expect("getUpdates")
            .len();
        eprintln!("soak: {pending} pending updates of the bot will be consumed by this run");
        foreign_pending = Some(pending);
        let stickers = api
            .get_forum_topic_icon_stickers()
            .await
            .expect("icon stickers");
        let (icons, _) =
            Icons::from_offered(stickers.into_iter().filter_map(|s| s.custom_emoji_id))
                .expect("icons");
        let options = Options {
            icons,
            chat_id: config.chat_id,
            grace: Duration::ZERO,
            ..Options::default()
        };
        (
            Some(api),
            Some(config.token),
            options,
            BucketConfig::default(),
            config.allowlist,
        )
    } else {
        let options = Options {
            chat_id: FAKE_CHAT,
            grace: Duration::ZERO,
            retry_every: Duration::from_secs(2),
            stream_every: Duration::from_millis(100),
            hold_answer: Duration::from_secs(1),
            stream_retry: Duration::from_millis(500),
            ..Options::default()
        };
        let bucket = BucketConfig {
            capacity: 4,
            refill_every: Duration::from_millis(150),
            min_gap: Duration::from_millis(30),
        };
        (
            None,
            None,
            options,
            bucket,
            [FAKE_USER].into_iter().collect(),
        )
    };
    let tg = Arc::new(Tg {
        live: live_api,
        calls: Mutex::new(Vec::new()),
        run: AtomicUsize::new(1),
        next_topic: AtomicI64::new(100),
        next_message: AtomicI64::new(1000),
        flood: Mutex::new(VecDeque::new()),
        streams_seen: AtomicUsize::new(0),
        retry_after: Duration::from_secs(1),
        updates,
        service: Mutex::new(Vec::new()),
    });
    let soak = Arc::new(Soak {
        live,
        token,
        root: root.clone(),
        home,
        config_dir,
        state,
        bin,
        agent_port,
        hook_port,
        tg: tg.clone(),
        bucket,
        options,
        allowlist,
        // Ids of simulated user messages; against the real chat they must
        // name no real message (the hub reacts to them with 👀).
        next_message: AtomicI64::new(if live { SYNTHETIC_MESSAGE } else { 50_000 }),
    });
    // The scenario runs as its own task, so a failed assertion inside it
    // comes back here as a `JoinError` and the cleanup below always runs:
    // its hub tasks are aborted and its stand-ins told to exit on unwind
    // (`Drop` of `Hub` and `Sim`).
    let outcome = {
        let soak = soak.clone();
        tokio::spawn(async move { scenario(&soak, started).await }).await
    };
    let undeleted = match &soak.token {
        Some(token) => delete_topics(token, soak.options.chat_id, &created_topics(&soak.tg)).await,
        None => Vec::new(),
    };
    // Stand-ins close their agents (up to 3 s) and exit; then the copy of
    // this binary can be removed.
    tokio::time::sleep(Duration::from_secs(4)).await;
    for _ in 0..40 {
        if std::fs::remove_dir_all(&soak.root).is_ok() || !soak.root.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if !undeleted.is_empty() {
        eprintln!("soak: topics of this run not deleted, delete them by hand: {undeleted:?}");
    }
    let mut report = match outcome {
        Ok(report) => report,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => panic!("the scenario was cancelled"),
    };
    assert!(
        undeleted.is_empty(),
        "topics of this run not deleted, delete them by hand: {undeleted:?}"
    );
    if let Some(pending) = foreign_pending {
        report.push_str(&format!(
            "- pending updates of the bot consumed at the start (written before the run, not seen by the stopped hub; at most 100 counted): {pending}\n"
        ));
    }
    report
}

/// Removes a folder when dropped, best effort.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Topics this run created, oldest first.
fn created_topics(tg: &Tg) -> Vec<i64> {
    tg.calls()
        .iter()
        .filter(|call| call.kind == "create_topic" && call.outcome == "ok")
        .filter_map(|call| call.message_id)
        .collect()
}

/// Live only: deletes `topics` (newest first) with a direct
/// `deleteForumTopic` call, waiting `retry_after` on 429; returns the ids it
/// could not delete. Errors are dropped unread: a transport error carries the
/// request URL, which holds the token.
async fn delete_topics(token: &BotToken, chat_id: i64, topics: &[i64]) -> Vec<i64> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://api.telegram.org/bot{}/deleteForumTopic",
        token.expose()
    );
    let mut undeleted = Vec::new();
    for &topic in topics.iter().rev() {
        let mut deleted = false;
        for _ in 0..5 {
            let body = json!({"chat_id": chat_id, "message_thread_id": topic});
            let answer = match client
                .post(&url)
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send()
                .await
            {
                Ok(response) => response.json::<Value>().await.unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            if answer["ok"] == true {
                deleted = true;
                break;
            }
            let wait = answer["parameters"]["retry_after"].as_u64().unwrap_or(1);
            tokio::time::sleep(Duration::from_secs(wait.max(1))).await;
        }
        if !deleted {
            undeleted.push(topic);
        }
    }
    undeleted
}

async fn scenario(soak: &Soak, started: Instant) -> String {
    let tg = soak.tg.clone();
    let (folder_a, folder_b) = (
        soak.root.join("work").join("A"),
        soak.root.join("work").join("B"),
    );
    let mut notes: Vec<String> = Vec::new();

    // ---- phase 1: three top-level sessions, three topics
    let hub = soak.start_hub(1).await;
    let mut a1 = soak.launch(A1, &folder_a).await;
    let mut a2 = soak.launch(A2, &folder_a).await;
    let mut b1 = soak.launch(B1, &folder_b).await;
    for sim in [&mut a1, &mut a2, &mut b1] {
        let done = sim.hook("SessionStart", json!({"source": "startup"})).await;
        assert!(
            !done["stderr"]
                .as_str()
                .unwrap_or_default()
                .contains("not delivered"),
            "{done}"
        );
        sim.start_agent();
    }
    let title = |sim: &Sim, ordinal| {
        topic_title(HOST, &folder_name(&sim.cwd), ordinal, Some(short(sim.id)))
    };
    let names = [title(&a1, 1), title(&a2, 2), title(&b1, 1)];
    wait_for("three topics", 30, || {
        names.iter().all(|name| topic_of(&tg, name).is_some())
    })
    .await;
    let t_a = topic_of(&tg, &names[0]).unwrap();
    let t_a2 = topic_of(&tg, &names[1]).unwrap();
    let t_b = topic_of(&tg, &names[2]).unwrap();
    let alive = soak.options.icons.alive.clone();
    wait_for("three agents bound", 30, || {
        [t_a, t_a2, t_b]
            .iter()
            .all(|&thread| last_icon(&tg, thread, 1) == alive)
    })
    .await;
    let owner: BTreeMap<&str, i64> = [
        (short(A1), t_a),
        (short(A2), t_a2),
        (short(B1), t_b),
        (short(NESTED), t_a),
        (short(A5), t_a),
    ]
    .into_iter()
    .collect();

    // ---- phase 2: routing both ways
    for (sim, thread) in [(&a1, t_a), (&a2, t_a2), (&b1, t_b)] {
        soak.say(&hub, thread, &format!("to {}", short(sim.id)));
    }
    for sim in [&a1, &a2, &b1] {
        let want = format!("to {}", short(sim.id));
        wait_for(&format!("inbound of {}", short(sim.id)), 20, || {
            sim.inbound().contains(&want)
        })
        .await;
    }
    for sim in [&mut a1, &mut a2, &mut b1] {
        let text = format!("reply from {}", short(sim.id));
        sim.rpc(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "reply", "arguments": {"text": text}}}));
        sim.append(&prompt_line(&format!("task {}", short(sim.id))));
        sim.append(&tool_lines(
            &format!("t-{}", short(sim.id)),
            &format!("step {}", short(sim.id)),
        ));
        sim.hook(
            "Stop",
            json!({"last_assistant_message": format!("answer {}", short(sim.id))}),
        )
        .await;
    }
    for (sim, thread) in [(&a1, t_a), (&a2, t_a2), (&b1, t_b)] {
        let s = short(sim.id);
        let want = [
            format!("reply from {s}"),
            format!("> task {s}"),
            format!("• Bash: step {s} ✓"),
            format!("answer {s}"),
        ];
        wait_for(&format!("topic lines of {s}"), 30, || {
            let got = tg.texts(thread);
            want.iter().all(|line| got.contains(line))
        })
        .await;
    }

    // ---- phase 3: a nested `claude -p` inside A1
    let nested_input = |event: &str, extra: Value| {
        let mut input = json!({
            "session_id": NESTED, "cwd": a1.cwd, "hook_event_name": event,
            "transcript_path": a1.transcript.with_file_name(format!("{NESTED}.jsonl")).to_string_lossy(),
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        input
    };
    let script = json!([
        {"hook": "SessionStart", "input": nested_input("SessionStart", json!({"source": "startup"}))},
        {"sleep_ms": 500},
        {"hook": "Stop", "input": nested_input("Stop", json!({"last_assistant_message": format!("nested answer {}", short(NESTED))}))},
        {"sleep_ms": 300},
        {"hook": "SessionEnd", "input": nested_input("SessionEnd", json!({"reason": "other"}))},
    ]);
    a1.send(json!({"nest": {"session": NESTED, "script": script}}));
    wait_for("nested run done", 30, || {
        a1.lines_of(A1)
            .iter()
            .any(|line| line["nest_done"] == NESTED)
    })
    .await;
    let nested_done = format!("nested answer {}", short(NESTED));
    wait_for("nested block with its answer", 30, || {
        tg.calls().iter().any(|call| {
            matches!(call.kind, "edit" | "send")
                && call.outcome == "ok"
                && call.text.contains(&nested_done)
        })
    })
    .await;
    let header = nested_header(NESTED);
    assert!(
        tg.texts(t_a).iter().any(|line| line == &header),
        "the nested block opens in its parent's topic"
    );
    let nested_kind = json!({"kind": "nested", "parent": A1});
    wait_for("nesting found through the process tree", 20, || {
        soak.registry()["sessions"][NESTED]["kind"] == nested_kind
    })
    .await;
    assert_eq!(tg.count("create_topic"), 3, "a nested run makes no topic");

    // ---- phase 4: a burst under the group limit, 429s, permission first
    let burst_a2 = if soak.live { 8 } else { 30 };
    let burst_b1 = if soak.live { 4 } else { 12 };
    let mut bytes = String::new();
    for n in 0..burst_a2 {
        bytes.push_str(&prompt_line(&format!("burst {} {n:02}", short(A2))));
    }
    for n in 0..burst_a2 {
        bytes.push_str(&tool_lines(
            &format!("m{n}"),
            &format!("merge {} {n:02}", short(A2)),
        ));
    }
    a2.append(&bytes);
    let mut bytes = String::new();
    for n in 0..burst_b1 {
        bytes.push_str(&prompt_line(&format!("burst {} {n:02}", short(B1))));
    }
    b1.append(&bytes);
    let burst_sent = |tg: &Tg| {
        tg.texts(t_a2)
            .iter()
            .filter(|line| line.starts_with(&format!("> burst {}", short(A2))))
            .count()
    };
    // The stream has queued the lines once the first ones went out.
    wait_for("burst under way", 30, || burst_sent(&tg) >= 2).await;
    let t0 = Instant::now();
    let sent_at_t0 = burst_sent(&tg);
    a1.rpc(json!({"jsonrpc": "2.0", "method": "notifications/claude/channel/permission_request",
        "params": {"request_id": "qwert", "tool_name": "Bash",
            "description": format!("soak {}", short(A1)), "input_preview": format!("echo {}", short(A1))}}));
    a2.rpc(json!({"jsonrpc": "2.0", "method": "notifications/claude/channel/permission_request",
        "params": {"request_id": "asdfg", "tool_name": "Bash",
            "description": format!("soak {}", short(A2)), "input_preview": format!("echo {}", short(A2))}}));
    let prompt_of = |thread: i64| {
        tg.calls().into_iter().find(|call| {
            call.kind == "permission" && call.outcome == "ok" && call.thread == Some(thread)
        })
    };
    wait_for("both permission prompts", 30, || {
        prompt_of(t_a).is_some() && prompt_of(t_a2).is_some()
    })
    .await;
    let (p_a, p_a2) = (prompt_of(t_a).unwrap(), prompt_of(t_a2).unwrap());
    // The 429s go into the rest of the burst, after the prompts: the
    // latencies above measure the priority of a prompt, not a 429 pause.
    if !soak.live {
        tg.arm_flood(&[2, 6]);
    }
    let latency_a = p_a.at.duration_since(t0);
    let latency_a2 = p_a2.at.duration_since(t0);
    soak.press(&hub, p_a.message_id.unwrap(), "allow:qwert");
    soak.press(&hub, p_a2.message_id.unwrap(), "deny:asdfg");
    wait_for("verdicts reach their agents", 30, || {
        a1.verdicts() == [("qwert".to_owned(), "allow".to_owned())]
            && a2.verdicts() == [("asdfg".to_owned(), "deny".to_owned())]
    })
    .await;
    assert!(b1.verdicts().is_empty());
    let want_a2: Vec<String> = (0..burst_a2)
        .map(|n| format!("> burst {} {n:02}", short(A2)))
        .chain((0..burst_a2).map(|n| format!("• Bash: merge {} {n:02} ✓", short(A2))))
        .collect();
    wait_for("the burst drained", 120, || {
        let got = tg.texts(t_a2);
        want_a2.iter().all(|line| got.contains(line))
    })
    .await;
    assert!(
        tg.flood.lock().unwrap().is_empty(),
        "both planned 429s fell into the burst"
    );
    let behind_a2 = tg
        .calls()
        .iter()
        .filter(|call| call.kind == "stream" && call.outcome == "ok" && call.at > p_a2.at)
        .flat_map(|call| call.text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|line| line.starts_with(&format!("> burst {}", short(A2))))
        .count();
    // Every burst line was in A2's transcript before the request was
    // written; `behind_a2` of them reached the topic after the prompt did.
    assert!(
        behind_a2 > 0,
        "the prompt of A2 overtook stream lines of its own topic written before it"
    );
    let got_a2: Vec<String> = tg
        .texts(t_a2)
        .into_iter()
        .filter(|line| line.contains("burst") || line.contains("merge"))
        .collect();
    let mut firsts = Vec::new();
    for line in got_a2 {
        if !firsts.contains(&line) {
            firsts.push(line);
        }
    }
    assert_eq!(firsts, want_a2, "FIFO within the topic, nothing lost");
    notes.push(format!(
        "burst: {} lines in topic A #2, {} in B; {} of A #2 sent before the prompts were asked",
        2 * burst_a2,
        burst_b1,
        sent_at_t0
    ));

    // ---- phase 5: A1 ends, the hub goes down, A5 starts, the hub returns
    a1.hook("SessionEnd", json!({"reason": "other"})).await;
    a1.close();
    wait_for("A1 ended in registry.json", 20, || {
        soak.registry()["sessions"][A1]["ended"] == true
    })
    .await;
    settle(&tg, Duration::from_millis(500), 60).await;
    drop(hub);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut a5 = soak.launch(A5, &folder_a).await;
    let down = a5.hook("SessionStart", json!({"source": "startup"})).await;
    assert!(
        down["stderr"]
            .as_str()
            .unwrap_or_default()
            .contains("kept for the next hook"),
        "{down}"
    );
    assert_eq!(soak.spool_files(A5), 1, "the missed start is kept");
    a5.start_agent();
    a5.append(&prompt_line(&format!("while down {}", short(A5))));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let creates_before = tg.count("create_topic");
    let hub = soak.start_hub(2).await;
    a5.hook(
        "UserPromptSubmit",
        json!({"prompt": "private prompt text", "prompt_id": "p5"}),
    )
    .await;
    wait_for("A5 takes the freed slot", 30, || {
        let registry = soak.registry();
        registry["slots"][0]["current_session"] == A5
    })
    .await;
    let sep = separator(A5, false);
    wait_for("the separator", 30, || tg.texts(t_a).contains(&sep)).await;
    wait_for("every agent bound again", 60, || {
        [t_a, t_a2, t_b]
            .iter()
            .all(|&thread| last_icon(&tg, thread, 2) == alive)
    })
    .await;
    wait_for("the spool is empty", 30, || soak.spool_files(A5) == 0).await;
    soak.say(&hub, t_a, &format!("to {}", short(A5)));
    soak.say(&hub, t_a2, &format!("to {} again", short(A2)));
    wait_for("inbound after the restart", 60, || {
        a5.inbound() == [format!("to {}", short(A5))]
            && a2.inbound().contains(&format!("to {} again", short(A2)))
    })
    .await;
    a5.hook(
        "Stop",
        json!({"last_assistant_message": format!("answer {}", short(A5))}),
    )
    .await;
    wait_for("A5's lines in topic A", 30, || {
        let got = tg.texts(t_a);
        got.contains(&format!("> while down {}", short(A5)))
            && got.contains(&format!("answer {}", short(A5)))
    })
    .await;
    settle(&tg, Duration::from_millis(1500), 90).await;

    // ---- final checks
    let calls = tg.calls();
    assert_eq!(tg.count("create_topic"), 3, "five runs, three topics");
    assert_eq!(creates_before, 3);
    assert_eq!(
        tg.texts(t_a).iter().filter(|line| **line == sep).count(),
        1,
        "one separator"
    );
    let separators: usize = [t_a, t_a2, t_b]
        .iter()
        .map(|&t| {
            tg.texts(t)
                .iter()
                .filter(|line| line.starts_with("── session"))
                .count()
        })
        .sum();
    assert_eq!(separators, 1, "no other separator anywhere");
    // No crossed route: every text naming a session is in that session's topic.
    for call in calls.iter().filter(|call| call.outcome == "ok") {
        let Some(thread) = call
            .thread
            .filter(|_| matches!(call.kind, "send" | "permission" | "stream"))
        else {
            continue;
        };
        for (session, &home) in &owner {
            if call.text.contains(session) {
                assert_eq!(
                    thread, home,
                    "{} shows in the wrong topic: {:?}",
                    session, call.text
                );
            }
        }
    }
    for sim in [&a2, &b1, &a5] {
        for text in sim.inbound() {
            assert!(
                text.contains(short(sim.id)),
                "{} got {text:?}",
                short(sim.id)
            );
        }
    }
    assert_eq!(a1.inbound(), [format!("to {}", short(A1))]);
    // Service messages: every forum_topic_edited of this run's topics was
    // deleted (live: the real queue may carry other topics' ones; the hub
    // never touches those, and they are not counted).
    let deleted: Vec<i64> = calls
        .iter()
        .filter(|call| call.kind == "delete" && call.outcome == "ok")
        .filter_map(|call| call.message_id)
        .collect();
    let own = [t_a, t_a2, t_b];
    let service: Vec<(Option<i64>, i64)> = tg
        .service
        .lock()
        .unwrap()
        .iter()
        .filter(|(thread, _)| thread.is_some_and(|thread| own.contains(&thread)))
        .copied()
        .collect();
    if soak.live {
        assert!(
            !service.is_empty(),
            "no forum_topic_edited seen: is another getUpdates consumer (the hub) running?"
        );
    } else {
        assert_eq!(service.len(), tg.count("edit_topic"), "one per topic edit");
    }
    let left: Vec<_> = service
        .iter()
        .filter(|(_, id)| !deleted.contains(id))
        .collect();
    assert!(
        left.is_empty(),
        "forum_topic_edited left in topics: {left:?}"
    );
    // 429s: the whole queue waited retry_after, the refused job went again
    // first, nothing was tried more than once again.
    let floods: Vec<&Call> = calls.iter().filter(|call| call.outcome == "429").collect();
    for flood in &floods {
        let next = calls
            .iter()
            .find(|call| call.at > flood.at)
            .expect("a call after the 429");
        assert!(
            next.at.duration_since(flood.at) >= tg.retry_after - Duration::from_millis(20),
            "a call {:?} after a 429",
            next.at.duration_since(flood.at)
        );
        let tries: Vec<&Call> = calls
            .iter()
            .filter(|call| {
                call.kind == flood.kind && call.thread == flood.thread && call.text == flood.text
            })
            .collect();
        assert_eq!(tries.len(), 2, "one retry, no storm: {tries:?}");
        assert_eq!(tries[1].outcome, "ok", "the retry went through");
    }
    if !soak.live {
        assert_eq!(floods.len(), 2, "both planned 429s happened");
    }
    // Metered sends within the bucket (per hub run, any window).
    let metered: Vec<(usize, Instant)> = calls
        .iter()
        .filter(|call| matches!(call.kind, "send" | "permission" | "stream" | "document"))
        .filter(|call| call.outcome == "ok")
        .map(|call| (call.run, call.at))
        .collect();
    let mut worst_window = 0.0f64;
    let mut min_gap = Duration::MAX;
    for (i, (run, at)) in metered.iter().enumerate() {
        if let Some((next_run, next_at)) = metered.get(i + 1)
            && next_run == run
        {
            min_gap = min_gap.min(next_at.duration_since(*at));
        }
        for window in [1, 3, 60].map(Duration::from_secs) {
            let inside = metered[i..]
                .iter()
                .take_while(|(r, t)| r == run && t.duration_since(*at) < window)
                .count() as f64;
            let allowed = f64::from(soak.bucket.capacity)
                + window.as_secs_f64() / soak.bucket.refill_every.as_secs_f64()
                + 1.0;
            worst_window = worst_window.max(inside / allowed);
            assert!(inside <= allowed, "{inside} metered sends in {window:?}");
        }
    }
    assert!(
        min_gap + Duration::from_millis(5) >= soak.bucket.min_gap,
        "sends closer than min_gap: {min_gap:?}"
    );
    // registry.json, field by field.
    let registry = soak.registry();
    let keys = |value: &Value| -> Vec<String> {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    };
    assert_eq!(
        keys(&registry),
        ["pids", "seq", "sessions", "slots", "subagents", "version"],
        "no other durable state"
    );
    assert_eq!(registry["version"], 1);
    assert!(registry["seq"].as_u64().is_some_and(|seq| seq > 0));
    assert_eq!(registry["subagents"], json!({}), "no subagent records");
    let slots = registry["slots"].as_array().expect("slots");
    assert_eq!(slots.len(), 3);
    for (slot, (folder, ordinal, thread, current)) in slots.iter().zip([
        (&a1.cwd, 1, t_a, A5),
        (&a2.cwd, 2, t_a2, A2),
        (&b1.cwd, 1, t_b, B1),
    ]) {
        assert_eq!(slot["host"], HOST);
        assert_eq!(slot["folder_name"], folder_name(folder));
        assert_eq!(slot["folder_key"], cctg::hub::registry::folder_key(folder));
        assert_eq!(slot["ordinal"], ordinal);
        assert_eq!(slot["topic_id"], thread);
        assert_eq!(slot["current_session"], current);
        assert_eq!(slot["pending_separator"], Value::Null);
        assert_eq!(
            slot["applied_title"],
            topic_title(HOST, &folder_name(folder), ordinal, Some(short(current)))
        );
        assert_eq!(slot["applied_icon"], json!(alive), "{current} alive");
        assert_eq!(
            keys(slot),
            [
                "applied_icon",
                "applied_title",
                "current_session",
                "folder_key",
                "folder_name",
                "host",
                "ordinal",
                "pending_separator",
                "topic_id"
            ],
            "no kept messages, nothing else"
        );
    }
    let sessions = registry["sessions"].as_object().expect("sessions");
    let mut ids: Vec<&str> = sessions.keys().map(String::as_str).collect();
    ids.sort_unstable();
    assert_eq!(ids, [A1, A2, A5, B1, NESTED]);
    let top = json!({"kind": "top_level"});
    let path = |path: &Path| json!(path.to_string_lossy());
    let mut seen = Vec::new();
    for (sim, slot, ended) in [
        (&a1, 0, true),
        (&a2, 1, false),
        (&b1, 2, false),
        (&a5, 0, false),
    ] {
        let (id, entry) = (sim.id, &sessions[sim.id]);
        assert_eq!(
            keys(entry),
            [
                "claude_pid",
                "ended",
                "host",
                "kind",
                "seen",
                "slot",
                "stream",
                "title",
                "transcript_path"
            ],
            "{id}"
        );
        assert_eq!(entry["host"], HOST);
        assert_eq!(entry["kind"], top, "{id}");
        assert_eq!(entry["slot"], slot, "{id}");
        assert_eq!(entry["ended"], ended, "{id}");
        assert_eq!(entry["claude_pid"], sim.pid, "{id}: the stand-in's pid");
        assert_eq!(entry["transcript_path"], path(&sim.transcript), "{id}");
        assert_eq!(
            entry["title"],
            Value::Null,
            "{id}: no ai-title line written"
        );
        seen.push(entry["seen"].as_u64().expect("seen"));
        // The stream read every line of the transcript and has no call open.
        let written = std::fs::metadata(&sim.transcript)
            .expect("transcript")
            .len();
        assert_eq!(entry["stream"]["offset"], written, "{id}: stream offset");
        assert_eq!(entry["stream"]["calls"], json!([]), "{id}: open calls");
        assert!(
            entry["stream"]["receipts"]
                .as_array()
                .is_some_and(|ids| ids.iter().all(Value::is_i64)),
            "{id}: receipts"
        );
    }
    let nested = &sessions[NESTED];
    assert_eq!(
        keys(nested),
        [
            "block",
            "claude_pid",
            "ended",
            "host",
            "kind",
            "seen",
            "slot",
            "title",
            "transcript_path"
        ],
        "a nested run has no stream"
    );
    assert_eq!(nested["host"], HOST);
    assert_eq!(nested["kind"], json!({"kind": "nested", "parent": A1}));
    assert_eq!(nested["slot"], 0, "nested runs point at the parent's slot");
    assert_eq!(nested["ended"], true);
    assert_eq!(nested["title"], Value::Null);
    assert_eq!(
        nested["transcript_path"],
        path(&a1.transcript.with_file_name(format!("{NESTED}.jsonl")))
    );
    assert!(
        nested["claude_pid"]
            .as_u64()
            .is_some_and(|pid| pid != u64::from(a1.pid)),
        "the nested stand-in's own pid"
    );
    seen.push(nested["seen"].as_u64().expect("seen"));
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 5, "every session has its own `seen` number");
    let block = &nested["block"];
    assert_eq!(block["header"], nested_header(NESTED));
    assert_eq!(
        block["thread_id"], t_a,
        "the block is in the parent's topic"
    );
    assert!(block["message_id"].is_i64());
    assert_eq!(
        (&block["running"], &block["sending"], &block["pending"]),
        (&json!(false), &json!(false), &Value::Null),
        "the block is finished and delivered"
    );
    let pids = registry["pids"].as_object().expect("pids");
    let mut want_pids = BTreeMap::new();
    for sim in [&a2, &b1, &a5] {
        want_pids.insert(format!("{HOST}/{}", sim.pid), json!(sim.id));
    }
    assert_eq!(
        pids.clone().into_iter().collect::<BTreeMap<_, _>>(),
        want_pids,
        "only running sessions keep a pid"
    );

    // ---- report
    let count = |kind: &str| {
        calls
            .iter()
            .filter(|call| call.kind == kind && call.outcome == "ok")
            .count()
    };
    let stream_lines: usize = calls
        .iter()
        .filter(|call| call.kind == "stream" && call.outcome == "ok")
        .map(|call| call.text.lines().count())
        .sum();
    let mut report = String::new();
    let mode = if soak.live {
        "live (real bot)"
    } else {
        "fake Telegram"
    };
    report.push_str(&format!(
        "# TASK-018 soak report\n\nmode: {mode}; duration {:.1} s; {} Telegram calls; {} hub runs\n\n",
        started.elapsed().as_secs_f64(),
        calls.len(),
        2
    ));
    report.push_str("| operation | Bot API method | accepted |\n|---|---|---|\n");
    for (kind, method) in [
        (
            "send",
            "sendMessage (replies, answers, notices, separators, blocks)",
        ),
        ("permission", "sendMessage (permission prompts)"),
        ("stream", "sendMessage (transcript stream)"),
        ("document", "sendDocument"),
        ("edit", "editMessageText"),
        ("react", "setMessageReaction"),
        ("callback", "answerCallbackQuery"),
        ("create_topic", "createForumTopic"),
        ("edit_topic", "editForumTopic"),
        ("delete", "deleteMessage (service messages)"),
    ] {
        report.push_str(&format!("| {kind} | {method} | {} |\n", count(kind)));
    }
    let errors = calls.iter().filter(|call| call.outcome == "error").count();
    let local = calls
        .iter()
        .filter(|call| call.outcome == "synthetic")
        .count();
    report.push_str(&format!(
        "\n- topics: {} (A, A #2, B); separators: {separators}; service messages: {} shown, {} deleted, {} left\n",
        count("create_topic"),
        service.len(),
        deleted.len(),
        left.len()
    ));
    report.push_str(&format!(
        "- 429: {} (retry_after {:?}), each followed by a pause of the whole queue and one retry; other errors: {errors}; answered locally (live: reactions and callback answers on simulated ids): {local}\n",
        floods.len(),
        tg.retry_after
    ));
    let paused = floods
        .iter()
        .any(|flood| flood.at < p_a2.at && flood.at + tg.retry_after > t0);
    report.push_str(&format!(
        "- permission latency (request written to the agent -> sendMessage): A {} ms (other topic), A #2 {} ms (own topic behind its stream){}; A #2 burst lines written before the request and sent after the prompt: {behind_a2}\n",
        latency_a.as_millis(),
        latency_a2.as_millis(),
        if paused { ", includes a 429 pause" } else { "" }
    ));
    report.push_str(&format!(
        "- stream: {stream_lines} lines in {} messages; metered sends peak at {:.0}% of the bucket in any 1, 3 or 60 s window; smallest gap {} ms (bucket {} + 1 per {} ms, min gap {} ms)\n",
        count("stream"),
        worst_window * 100.0,
        min_gap.as_millis(),
        soak.bucket.capacity,
        soak.bucket.refill_every.as_millis(),
        soak.bucket.min_gap.as_millis()
    ));
    report.push_str(
        "- edits and topic calls are counted separately and are not compared with the 20 messages/min group limit: Telegram publishes no number for them\n",
    );
    for note in notes {
        report.push_str(&format!("- {note}\n"));
    }
    report.push_str(&format!(
        "- registry.json: 3 slots (A: {}, A #2: {}, B: {}), nested {} -> parent {}\n",
        short(A5),
        short(A2),
        short(B1),
        short(NESTED),
        short(A1)
    ));
    drop(hub);
    drop((a1, a2, b1, a5));
    report
}

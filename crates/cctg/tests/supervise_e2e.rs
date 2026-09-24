//! `cctg supervise` and `cctg deploy` with real processes (TASK-026).
//!
//! A copy of the built `cctg` in a temp bin directory runs `supervise`; its
//! hubs talk to a fake Bot API (`CCTG_BOT_API_URL`) served by this test. Home,
//! hub state and `CLAUDE_CONFIG_DIR` live in the temp directory; the real
//! `~/.cctg` and `~/.claude` are never read or written, no window opens.
//!
//! One scenario, in order:
//! 1. the first two hubs fail `getMe` and exit; the supervisor restarts them
//!    after 1 s and 2 s;
//! 2. a session starts (hook POST), gets a topic, and a real `cctg agent`
//!    receives a topic message;
//! 3. `cctg deploy` of a different build: `deployed`, the binary is swapped,
//!    no second topic (the registry survived), the agent reconnects and gets
//!    the next message;
//! 4. a candidate whose hub exits at once (a copy of this test binary, see
//!    `main`): `rolled back`, exit 1, the previous binary runs again;
//! 5. a file that is no program: `rejected`, nothing changes;
//! 6. the running binary again: `unchanged`;
//!    (Windows) a swap that fails (`cctg.old` held open): `failed`, exit 1,
//!    the candidate is withdrawn and the hub restarts once, not in a loop;
//! 7. Ctrl+Break (SIGTERM on Unix): the supervisor stops, its hub stops
//!    gracefully (registry written), nothing listens any more.
//!
//! About 30 s. Runs in the normal `cargo test`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::wire::{HookEvent, HookPost, Secret};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

mod common;

const SECRET: &str = "supervise-e2e-secret-0123456789";
const TOKEN: &str = "123456:supervise-e2e-token";
const CHAT: i64 = -1000000000001;
const USER: i64 = 1001;
const SESSION: &str = "5e9e0000-0000-4000-8000-000000000026";
const WAIT: Duration = Duration::from_secs(40);
const EXE: &str = std::env::consts::EXE_SUFFIX;

/// The supervisor's stderr (its hubs' too), printed when a check fails.
static SUPERVISOR_LOG: std::sync::OnceLock<Arc<Mutex<String>>> = std::sync::OnceLock::new();

fn main() {
    // A copy of this binary is the candidate whose hub crashes.
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("cctg 0.0.0-crashing");
            return;
        }
        Some("hub") => std::process::exit(3),
        _ => {}
    }
    if std::env::args().any(|arg| arg == "--list") {
        println!("supervise_e2e: test");
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let outcome = std::panic::catch_unwind(|| runtime.block_on(scenario()));
    if outcome.is_err() {
        if let Some(log) = SUPERVISOR_LOG.get() {
            eprintln!(
                "--- supervisor log ---
{}",
                log.lock().unwrap()
            );
        }
        std::process::exit(101);
    }
    println!("supervise_e2e: ok");
}

// ---------------------------------------------------------------- fake Bot API

#[derive(Default)]
struct FakeState {
    calls: Vec<(Instant, String)>,
    me_failures: usize,
    updates: Vec<Value>,
    next_update: i64,
    topics: i64,
    messages: i64,
}

#[derive(Default)]
struct Fake {
    state: Mutex<FakeState>,
    new_update: Notify,
}

impl Fake {
    fn calls_of(&self, method: &str) -> Vec<Instant> {
        let state = self.state.lock().unwrap();
        state
            .calls
            .iter()
            .filter(|(_, name)| name == method)
            .map(|(at, _)| *at)
            .collect()
    }

    fn push_message(&self, thread_id: i64, text: &str) {
        let mut state = self.state.lock().unwrap();
        state.next_update += 1;
        let id = state.next_update;
        state.updates.push(json!({
            "update_id": id,
            "message": {
                "message_id": 5000 + id,
                "message_thread_id": thread_id,
                "is_topic_message": true,
                "date": 1,
                "text": text,
                "from": { "id": USER, "is_bot": false, "first_name": "u" },
                "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
            },
        }));
        drop(state);
        self.new_update.notify_waiters();
    }

    /// `(http status, body)` for one Bot API call.
    async fn answer(&self, method: &str, body: &Value) -> (u16, Value) {
        {
            let mut state = self.state.lock().unwrap();
            state.calls.push((Instant::now(), method.to_owned()));
            if method == "getMe" && state.me_failures > 0 {
                state.me_failures -= 1;
                return (
                    401,
                    json!({"ok": false, "error_code": 401, "description": "Unauthorized"}),
                );
            }
        }
        let ok = |result: Value| (200, json!({"ok": true, "result": result}));
        match method {
            "getMe" => {
                ok(json!({"id": 3003, "is_bot": true, "first_name": "b", "username": "fake_bot"}))
            }
            "getChatMember" => ok(json!({
                "status": "administrator", "can_manage_topics": true, "can_delete_messages": true,
            })),
            "getForumTopicIconStickers" => ok(json!(
                [
                    cctg::hub::registry::ICON_ALIVE,
                    cctg::hub::registry::ICON_DEAD,
                    cctg::hub::registry::ICON_WAITING,
                    cctg::hub::registry::ICON_NO_CHANNEL,
                ]
                .iter()
                .map(|id| json!({"custom_emoji_id": id, "emoji": "x"}))
                .collect::<Vec<_>>()
            )),
            "getUpdates" => ok(Value::Array(self.updates(body).await)),
            "createForumTopic" => {
                let mut state = self.state.lock().unwrap();
                state.topics += 1;
                ok(json!({
                    "message_thread_id": 100 + state.topics,
                    "name": body["name"],
                    "icon_color": 0,
                }))
            }
            "sendMessage" | "editMessageText" => {
                let mut state = self.state.lock().unwrap();
                state.messages += 1;
                ok(json!({
                    "message_id": state.messages,
                    "message_thread_id": body["message_thread_id"],
                    "date": 1,
                    "chat": { "id": CHAT },
                }))
            }
            _ => ok(json!(true)),
        }
    }

    /// Updates at or past the offset, waiting up to a second for one.
    async fn updates(&self, body: &Value) -> Vec<Value> {
        let offset = body["offset"].as_i64().unwrap_or(0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let waiting = self.new_update.notified();
            let found: Vec<Value> = {
                let state = self.state.lock().unwrap();
                state
                    .updates
                    .iter()
                    .filter(|update| update["update_id"].as_i64().unwrap_or(0) >= offset)
                    .cloned()
                    .collect()
            };
            if !found.is_empty() {
                return found;
            }
            if tokio::time::timeout_at(deadline, waiting).await.is_err() {
                return Vec::new();
            }
        }
    }
}

async fn serve_fake(fake: Arc<Fake>) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let fake = fake.clone();
            tokio::spawn(async move {
                let _ = handle_http(stream, &fake).await;
            });
        }
    });
    port
}

/// One request per connection, answered with `Connection: close`.
async fn handle_http(mut stream: tokio::net::TcpStream, fake: &Fake) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    let method = path.rsplit('/').next().unwrap_or_default().to_owned();
    let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
    let (status, answer) = fake.answer(&method, &body).await;
    let answer = answer.to_string();
    let reason = if status == 200 { "OK" } else { "Unauthorized" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

// ------------------------------------------------------------------ processes

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// Kills its process on drop (a failed check must not leave one behind).
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// A command without any `CCTG_*` or Claude Code session variable of the
/// environment this test runs in; home is `home`.
fn clean_command(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    common::isolate(&mut command, home);
    command
}

/// Collects a child's stderr lines.
fn collect(mut stream: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let log = Arc::new(Mutex::new(String::new()));
    let sink = log.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    log
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Runs `cctg deploy` from the bin directory; returns (success, stdout).
async fn deploy(exe: &Path, candidate: &Path, home: &Path) -> (bool, String) {
    let mut command = clean_command(exe, home);
    command
        .arg("deploy")
        .arg(candidate)
        .args(["--timeout-secs", "90"])
        .stdin(Stdio::null());
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .expect("cctg deploy runs");
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    eprintln!("deploy: {stdout}");
    (output.status.success(), stdout)
}

fn registry_has_session(state: &Path) -> bool {
    std::fs::read_to_string(state.join("registry.json")).is_ok_and(|text| text.contains(SESSION))
}

fn stop_supervisor(child: &Child) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        // SAFETY: plain FFI call with a process group id; no pointers.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
        assert_ne!(sent, 0, "Ctrl+Break could not be sent to the supervisor");
    }
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .expect("kill");
        assert!(status.success());
    }
}

// ------------------------------------------------------------------- scenario

async fn scenario() {
    let root =
        Root(std::env::temp_dir().join(format!("cctg-supervise-e2e-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let [bin, state, home, work, build] =
        ["bin", "state", "home", "work", "build"].map(|name| root.0.join(name));
    for dir in [&bin, &state, &home.join(".cctg"), &work, &build] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let exe = bin.join(format!("cctg{EXE}"));
    let original = std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap();
    std::fs::write(&exe, &original).unwrap();

    let fake = Arc::new(Fake::default());
    fake.state.lock().unwrap().me_failures = 2;
    let api_port = serve_fake(fake.clone()).await;
    let (agent_port, hook_port) = (free_port(), free_port());
    std::fs::write(
        home.join(".cctg").join("device.env"),
        format!(
            "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR=127.0.0.1:{hook_port}\n\
             CCTG_HUB_AGENT_ADDR=127.0.0.1:{agent_port}\nCCTG_HOST=box\n"
        ),
    )
    .unwrap();

    // 1. The supervisor, and restarts after a failing start.
    let mut command = clean_command(&exe, &home);
    command
        .args(["supervise", "--trial-secs", "3"])
        .current_dir(&work)
        .env("CCTG_BOT_TOKEN", TOKEN)
        .env("CCTG_CHAT_ID", CHAT.to_string())
        .env("CCTG_ALLOWED_USER_IDS", USER.to_string())
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_STATE_DIR", &state)
        .env("CCTG_PROJECTS_DIR", root.0.join("projects"))
        .env("CCTG_AGENT_LISTEN", format!("127.0.0.1:{agent_port}"))
        .env("CCTG_HOOK_LISTEN", format!("127.0.0.1:{hook_port}"))
        .env("CCTG_BOT_API_URL", format!("http://127.0.0.1:{api_port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Its own group: Ctrl+Break goes to it alone, not to this test.
        command.creation_flags(0x0000_0200);
    }
    let mut supervisor = Proc(command.spawn().expect("cctg supervise"));
    let log = collect(supervisor.0.stderr.take().unwrap());
    let _ = SUPERVISOR_LOG.set(log.clone());

    wait_for("a polling hub", || !fake.calls_of("getUpdates").is_empty()).await;
    let starts = fake.calls_of("getMe");
    assert_eq!(starts.len(), 3, "two failed starts, then one that polls");
    let gaps = [starts[1] - starts[0], starts[2] - starts[1]];
    assert!(
        gaps[0] >= Duration::from_millis(900),
        "first restart after ~1 s: {gaps:?}"
    );
    assert!(
        gaps[1] >= Duration::from_millis(1900),
        "second restart after ~2 s: {gaps:?}"
    );

    // 2. A session with a topic and a live agent.
    let start = HookPost::new(
        "box".to_owned(),
        SESSION.to_owned(),
        work.to_string_lossy().into_owned(),
        root.0
            .join("projects")
            .join("p")
            .join(format!("{SESSION}.jsonl"))
            .to_string_lossy()
            .into_owned(),
        HookEvent::SessionStart {
            source: Some("startup".to_owned()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        },
    );
    cctg::hook::post(
        &format!("127.0.0.1:{hook_port}"),
        &Secret::parse(SECRET).unwrap(),
        &start,
        Duration::from_secs(5),
    )
    .await
    .expect("SessionStart accepted");
    wait_for("the session's topic", || {
        fake.calls_of("createForumTopic").len() == 1
    })
    .await;
    wait_for("the saved registry", || registry_has_session(&state)).await;
    let thread_id = 101;

    let mut agent_command = clean_command(Path::new(env!("CARGO_BIN_EXE_cctg")), &home);
    agent_command
        .arg("agent")
        .current_dir(&work)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", home.join("claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut agent = Proc(agent_command.spawn().expect("cctg agent"));
    let mut agent_in = agent.0.stdin.take().unwrap();
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    ] {
        writeln!(agent_in, "{line}").unwrap();
    }
    agent_in.flush().unwrap();
    let agent_out = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let lines = agent_out.clone();
        let stdout = agent.0.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                lines.lock().unwrap().push(line);
            }
        });
    }
    let delivered =
        |text: &'static str| {
            let lines = agent_out.clone();
            move || {
                lines.lock().unwrap().iter().any(|line| {
                    line.contains("notifications/claude/channel") && line.contains(text)
                })
            }
        };
    fake.push_message(thread_id, "before-deploy");
    wait_for("the first message at the agent", delivered("before-deploy")).await;

    // 3. A good deploy.
    let mut good = original.clone();
    good.extend_from_slice(b"\0supervise-e2e candidate");
    let good_path = build.join(format!("good{EXE}"));
    std::fs::write(&good_path, &good).unwrap();
    let (ok, out) = deploy(&exe, &good_path, &home).await;
    assert!(ok && out.starts_with("deployed: cctg "), "{out}");
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        good,
        "the new binary is in place"
    );
    assert_eq!(
        std::fs::read(bin.join(format!("cctg.old{EXE}"))).unwrap(),
        original,
        "the old binary is kept"
    );
    assert!(!bin.join(format!("cctg.next{EXE}")).exists());
    assert!(registry_has_session(&state));
    fake.push_message(thread_id, "after-deploy");
    wait_for(
        "a message at the reconnected agent",
        delivered("after-deploy"),
    )
    .await;
    assert_eq!(
        fake.calls_of("createForumTopic").len(),
        1,
        "the new hub knew the slot: the registry survived"
    );

    // 4. A candidate whose hub exits at once.
    let crashing = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let crashing_path = build.join(format!("crashing{EXE}"));
    std::fs::write(&crashing_path, &crashing).unwrap();
    let (ok, out) = deploy(&exe, &crashing_path, &home).await;
    assert!(
        !ok && out.starts_with("rolled back: the new hub exited"),
        "{out}"
    );
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        good,
        "the previous binary is back"
    );
    assert_eq!(
        std::fs::read(bin.join(format!("cctg.bad{EXE}"))).unwrap(),
        crashing,
        "the rolled back binary is kept"
    );
    fake.push_message(thread_id, "after-rollback");
    wait_for("a message after the rollback", delivered("after-rollback")).await;

    // 5. No program at all.
    let garbage_path = build.join(format!("garbage{EXE}"));
    std::fs::write(&garbage_path, b"not a program").unwrap();
    let (ok, out) = deploy(&exe, &garbage_path, &home).await;
    assert!(!ok && out.starts_with("rejected: "), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), good);

    // 6. The running binary once more.
    let (ok, out) = deploy(&exe, &good_path, &home).await;
    assert!(ok && out.starts_with("unchanged"), "{out}");
    assert_eq!(fake.calls_of("createForumTopic").len(), 1);

    // 6b. A swap that fails: the candidate leaves, no restart loop.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let old = bin.join(format!("cctg.old{EXE}"));
        std::fs::write(&old, b"held").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&old)
            .unwrap();
        let mut other = original.clone();
        other.extend_from_slice(b"\0supervise-e2e swap fails");
        let other_path = build.join(format!("other{EXE}"));
        std::fs::write(&other_path, &other).unwrap();
        let starts = fake.calls_of("getMe").len();
        let (ok, out) = deploy(&exe, &other_path, &home).await;
        assert!(!ok && out.starts_with("failed: "), "{out}");
        assert!(!bin.join(format!("cctg.next{EXE}")).exists(), "withdrawn");
        assert_eq!(std::fs::read(&exe).unwrap(), good);
        wait_for("the hub after the failed swap", || {
            fake.calls_of("getMe").len() > starts
        })
        .await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(
            fake.calls_of("getMe").len(),
            starts + 1,
            "one restart, no loop"
        );
        drop(held);
        std::fs::remove_file(&old).unwrap();
    }

    // 7. Ctrl+Break stops the supervisor and, gracefully, its hub.
    let stops_before = log.lock().unwrap().matches("slot registry saved").count();
    stop_supervisor(&supervisor.0);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = supervisor.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the supervisor stopped");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "{status:?}");
    wait_for("the hub's listener to close", || {
        TcpStream::connect_timeout(
            &(Ipv4Addr::LOCALHOST, hook_port).into(),
            Duration::from_millis(300),
        )
        .is_err()
    })
    .await;
    // Give the log reader a moment for the last lines.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let log = log.lock().unwrap().clone();
    assert!(
        log.matches("slot registry saved").count() > stops_before,
        "the last hub stopped gracefully:\n{log}"
    );
    assert!(registry_has_session(&state));
    assert!(
        !log.contains(SECRET) && !log.contains(TOKEN),
        "no secrets in logs"
    );
    // Graceful stops: one per deploy that stopped a hub, and the last one.
    assert!(log.matches("slot registry saved").count() >= 3, "{log}");
    drop(agent);
}

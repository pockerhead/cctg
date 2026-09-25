//! TLS between devices and the hub (TASK-035), with real processes: a real
//! `cctg hub` with a self-signed certificate made here, a fake Bot API
//! served by this test, a real `cctg hook SessionStart` and a real
//! `cctg agent`, both reading the pin from a temp `device.env`.
//!
//! Checks, in order: `cctg health` sees the hub; a hook with another pin
//! and a hook without a pin reach nothing (no topic); the pinned hook
//! creates the session's topic; the pinned agent registers and a topic
//! message arrives in its channel; the hub logs its certificate's sha256 and
//! never the secret or the token; a stopped hub fails `cctg health`.
//! Everything listens on loopback only (no firewall prompt); the container
//! run in `docs/remote-hub.md` covers another machine.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::tls::CertPin;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

mod common;

const SECRET: &str = "tls-e2e-secret-0123456789abcdef";
const TOKEN: &str = "123456:tls-e2e-token-value";
const CHAT: i64 = -1000000000001;
const USER: i64 = 1001;
const SESSION: &str = "5e550000-0000-4000-8000-000000000035";
const WAIT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- fake Bot API

#[derive(Default)]
struct Fake {
    state: Mutex<FakeState>,
    new_update: Notify,
}

#[derive(Default)]
struct FakeState {
    calls: Vec<String>,
    updates: Vec<Value>,
    topics: i64,
    messages: i64,
}

impl Fake {
    fn calls_of(&self, method: &str) -> usize {
        let state = self.state.lock().unwrap();
        state.calls.iter().filter(|name| *name == method).count()
    }

    fn push_message(&self, thread_id: i64, text: &str) {
        let mut state = self.state.lock().unwrap();
        let id = state.updates.len() as i64 + 1;
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

    async fn answer(&self, method: &str, body: &Value) -> Value {
        self.state.lock().unwrap().calls.push(method.to_owned());
        let ok = |result: Value| json!({"ok": true, "result": result});
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
    let answer = fake.answer(&method, &body).await.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
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

/// A home whose `device.env` points at the hub, with `pin` when given.
fn device_home(root: &Path, name: &str, ports: (u16, u16), pin: Option<&str>) -> PathBuf {
    let home = root.join(name);
    std::fs::create_dir_all(home.join(".cctg")).unwrap();
    let (agent, hook) = ports;
    let mut env = format!(
        "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_AGENT_ADDR=127.0.0.1:{agent}\n\
         CCTG_HUB_HOOK_ADDR=127.0.0.1:{hook}\nCCTG_HOST=box\n"
    );
    if let Some(pin) = pin {
        env.push_str(&format!("CCTG_HUB_CERT_SHA256={pin}\n"));
    }
    std::fs::write(home.join(".cctg").join("device.env"), env).unwrap();
    home
}

/// Runs a real `cctg hook SessionStart`; returns its stderr.
async fn session_start(home: &Path, work: &Path) -> String {
    let input = json!({
        "session_id": SESSION,
        "cwd": work,
        "transcript_path": work.join(format!("{SESSION}.jsonl")),
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    let mut command = common::cctg(home);
    command
        .args(["hook", "SessionStart"])
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::task::spawn_blocking(move || {
        let mut child = command.spawn().expect("cctg hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().expect("cctg hook ends")
    })
    .await
    .unwrap();
    assert!(output.status.success(), "a hook always exits 0");
    assert!(output.stdout.is_empty(), "a hook prints nothing on stdout");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

async fn health(home: &Path, ports: (u16, u16)) -> bool {
    let mut command = common::cctg(home);
    command
        .arg("health")
        .env("CCTG_AGENT_LISTEN", format!("127.0.0.1:{}", ports.0))
        .env("CCTG_HOOK_LISTEN", format!("127.0.0.1:{}", ports.1))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tokio::task::spawn_blocking(move || command.status().expect("cctg health"))
        .await
        .unwrap()
        .success()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_reach_the_hub_only_over_pinned_tls() {
    let root = Root(std::env::temp_dir().join(format!("cctg-tls-e2e-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let [state, hub_home, work, tls] = ["state", "hub-home", "work", "tls"].map(|n| root.0.join(n));
    for dir in [&state, &hub_home, &work, &tls] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    std::fs::write(tls.join("cert.pem"), cert.pem()).unwrap();
    std::fs::write(tls.join("key.pem"), signing_key.serialize_pem()).unwrap();
    let pin = CertPin::of(cert.der());
    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let other_pin = CertPin::of(other.cert.der());

    let fake = Arc::new(Fake::default());
    let api_port = serve_fake(fake.clone()).await;
    let ports = (free_port(), free_port());
    let mut hub_command = common::cctg(&hub_home);
    hub_command
        .arg("hub")
        .current_dir(&state)
        .env("CCTG_BOT_TOKEN", TOKEN)
        .env("CCTG_CHAT_ID", CHAT.to_string())
        .env("CCTG_ALLOWED_USER_IDS", USER.to_string())
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_STATE_DIR", &state)
        .env("CCTG_AGENT_LISTEN", format!("127.0.0.1:{}", ports.0))
        .env("CCTG_HOOK_LISTEN", format!("127.0.0.1:{}", ports.1))
        .env("CCTG_BOT_API_URL", format!("http://127.0.0.1:{api_port}"))
        .env("CCTG_TLS_CERT", tls.join("cert.pem"))
        .env("CCTG_TLS_KEY", tls.join("key.pem"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut hub = Proc(hub_command.spawn().expect("cctg hub"));
    let hub_log = collect(hub.0.stderr.take().unwrap());
    wait_for("a polling hub", || fake.calls_of("getUpdates") > 0).await;
    assert!(health(&hub_home, ports).await, "cctg health sees the hub");

    // Another certificate's pin, and no pin at all: nothing arrives.
    let wrong = device_home(&root.0, "wrong-pin", ports, Some(&other_pin.to_string()));
    let stderr = session_start(&wrong, &work).await;
    assert!(stderr.contains("not delivered"), "{stderr}");
    assert!(!stderr.contains(SECRET));
    let plain = device_home(&root.0, "no-pin", ports, None);
    let stderr = session_start(&plain, &work).await;
    assert!(stderr.contains("not delivered"), "{stderr}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.calls_of("createForumTopic"), 0, "no topic without TLS");

    // The pinned hook, with the whole openssl line quoted (unquoted, the
    // space would break the env file).
    let good = device_home(
        &root.0,
        "device",
        ports,
        Some(&format!("\"sha256 Fingerprint={pin}\"")),
    );
    let stderr = session_start(&good, &work).await;
    assert!(stderr.is_empty(), "the pinned hook is quiet: {stderr}");
    wait_for("the session's topic", || {
        fake.calls_of("createForumTopic") == 1
    })
    .await;

    // The pinned agent: registered over TLS, a topic message reaches it.
    let mut agent_command = common::cctg(&good);
    agent_command
        .arg("agent")
        .current_dir(&work)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", good.join("claude"))
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
    wait_for("the agent's registration", || {
        hub_log.lock().unwrap().contains("agent registered")
    })
    .await;
    fake.push_message(101, "over-tls");
    wait_for("the message at the agent", || {
        agent_out
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("notifications/claude/channel") && line.contains("over-tls"))
    })
    .await;

    let log = hub_log.lock().unwrap().clone();
    assert!(log.contains("TLS certificate loaded"), "{log}");
    assert!(log.contains(&pin.to_string()), "the pin is logged: {log}");
    assert!(!log.contains(SECRET), "the secret is never logged");
    assert!(
        !log.contains("tls-e2e-token-value"),
        "the token is never logged"
    );
    drop(agent_in);
    drop(agent);

    let _ = hub.0.kill();
    let _ = hub.0.wait();
    assert!(
        !health(&hub_home, ports).await,
        "a stopped hub is unhealthy"
    );
}

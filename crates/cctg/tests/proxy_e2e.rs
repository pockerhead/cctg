//! The hub behind an HTTP proxy of its environment (TASK-035 decision 2: a
//! server that reaches Telegram only through a local proxy). A real
//! `cctg hub` with `HTTPS_PROXY` / `HTTP_PROXY` naming a proxy stand-in
//! served here, whose URL carries a user and a password.
//!
//! 1. `https://` Bot API: the hub asks the proxy to `CONNECT` to the Bot API
//!    host, with the proxy credentials; the stand-in refuses, the hub gives up
//!    after its start retries. The host is `.invalid`: without the proxy
//!    nothing could reach it, and no real Telegram is ever contacted.
//! 2. `http://` Bot API on a loopback port where nothing listens: every
//!    request goes to the proxy in absolute form, the stand-in answers as the
//!    Bot API, and the hub starts polling.
//!
//! Neither the proxy's password nor its user reach the hub's output.

use std::io::Read;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;

const SECRET: &str = "proxy-e2e-secret-0123456789abc";
const TOKEN: &str = "123456:proxy-e2e-token";
const USER: &str = "cctgproxyuser";
const PASSWORD: &str = "proxy-pass-marker-42";
const WAIT: Duration = Duration::from_secs(30);

/// What the stand-in saw: request lines and whether they carried
/// `Proxy-Authorization`.
#[derive(Default)]
struct Seen(Mutex<Vec<(String, bool)>>);

impl Seen {
    fn lines(&self) -> Vec<(String, bool)> {
        self.0.lock().unwrap().clone()
    }
}

async fn serve_proxy(seen: Arc<Seen>) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let _ = proxy_request(stream, &seen).await;
            });
        }
    });
    port
}

/// One request per connection: `CONNECT` is refused, an absolute-form
/// request is answered as the Bot API.
async fn proxy_request(mut stream: TcpStream, seen: &Seen) -> std::io::Result<()> {
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
    let request_line = head.lines().next().unwrap_or_default().to_owned();
    let authorized = head.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("proxy-authorization: basic ")
    });
    seen.0
        .lock()
        .unwrap()
        .push((request_line.clone(), authorized));
    if request_line.starts_with("CONNECT ") {
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return stream.shutdown().await;
    }
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
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let method = target.rsplit('/').next().unwrap_or_default();
    let answer = bot_api(method).to_string();
    if method == "getUpdates" {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn bot_api(method: &str) -> Value {
    let ok = |result: Value| json!({"ok": true, "result": result});
    match method {
        "getMe" => {
            ok(json!({"id": 3003, "is_bot": true, "first_name": "b", "username": "fake_bot"}))
        }
        "getChatMember" => ok(json!({"status": "administrator", "can_manage_topics": true})),
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
        "getUpdates" => ok(json!([])),
        _ => ok(json!(true)),
    }
}

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// A real hub with the proxy variable `proxy_var` and the Bot API `api`.
fn hub(
    root: &Root,
    name: &str,
    proxy_var: &str,
    proxy_port: u16,
    api: &str,
) -> (Proc, Arc<Mutex<String>>) {
    let state = root.0.join(name);
    std::fs::create_dir_all(&state).unwrap();
    let mut command = common::cctg(&state);
    for var in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(var);
    }
    command
        .arg("hub")
        .current_dir(&state)
        .env(
            proxy_var,
            format!("http://{USER}:{PASSWORD}@127.0.0.1:{proxy_port}"),
        )
        .env("CCTG_BOT_TOKEN", TOKEN)
        .env("CCTG_CHAT_ID", "-1000000000001")
        .env("CCTG_ALLOWED_USER_IDS", "1001")
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_STATE_DIR", &state)
        .env("CCTG_AGENT_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("CCTG_HOOK_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("CCTG_BOT_API_URL", api)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = Proc(command.spawn().expect("cctg hub"));
    let log = collect(child.0.stderr.take().unwrap());
    (child, log)
}

async fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_private(log: &str) {
    assert!(
        !log.contains(PASSWORD),
        "the proxy password in the output: {log}"
    );
    assert!(!log.contains(USER), "the proxy user in the output: {log}");
    assert!(
        !log.contains("proxy-e2e-token"),
        "the token in the output: {log}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hub_reaches_the_bot_api_only_through_its_proxy() {
    let root = Root(std::env::temp_dir().join(format!("cctg-proxy-e2e-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let seen = Arc::new(Seen::default());
    let proxy = serve_proxy(seen.clone()).await;

    // 1. https: a CONNECT with the credentials, then the hub gives up.
    let (mut tunnel, log) = hub(
        &root,
        "connect",
        "HTTPS_PROXY",
        proxy,
        "https://api.telegram.invalid",
    );
    let exited = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = tunnel.0.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await
    .unwrap();
    assert!(
        exited.is_some_and(|status| !status.success()),
        "the hub gave up"
    );
    let connects: Vec<_> = seen
        .lines()
        .into_iter()
        .filter(|(line, _)| line.starts_with("CONNECT api.telegram.invalid:443 "))
        .collect();
    assert!(!connects.is_empty(), "{:?}", seen.lines());
    assert!(
        connects.iter().all(|(_, authorized)| *authorized),
        "credentials go to the proxy"
    );
    let text = log.lock().unwrap().clone();
    assert!(
        text.contains("through the proxy of the environment"),
        "{text}"
    );
    assert_private(&text);

    // 2. http: absolute-form requests; the stand-in is the Bot API.
    let dead = free_port();
    let (_polling, log) = hub(
        &root,
        "forward",
        "HTTP_PROXY",
        proxy,
        &format!("http://127.0.0.1:{dead}"),
    );
    wait_for("polling through the proxy", || {
        seen.lines().iter().any(|(line, authorized)| {
            *authorized
                && line.starts_with(&format!("POST http://127.0.0.1:{dead}/bot"))
                && line.contains("/getUpdates ")
        })
    })
    .await;
    wait_for("the start line", || {
        log.lock().unwrap().contains("hub started, polling")
    })
    .await;
    assert_private(&log.lock().unwrap());
}

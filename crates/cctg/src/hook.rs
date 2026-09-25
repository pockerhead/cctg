//! `cctg hook <event>`: read the Claude Code hook input from stdin, keep only
//! the fields the hub uses, and send them in one HTTP POST. The hook always
//! exits 0 and never writes to stdout (for `SessionStart` and
//! `UserPromptSubmit` stdout would become context for Claude). Failures go to
//! stderr as fixed texts: never the input, never the secret.
//!
//! Transport: hand-written HTTP/1.1 instead of `reqwest`: the body is tiny
//! and a full HTTP client costs start-up time the `SessionEnd` budget (1.5 s
//! shared) cannot spare. Plain TCP to a hub on this machine; TLS with the
//! pinned hub certificate to any other ([`crate::tls`], TASK-035), with
//! longer budgets for the extra round trip.
//!
//! [`HookPost::new`](crate::wire::HookPost::new) mints the event id; calling
//! [`post`] again with the same value re-sends the same event, which the hub
//! drops as a repeat.
//!
//! A session start or end the hub did not take is kept in the device spool
//! ([`crate::spool`]); every hook of the same session first sends what its
//! session has kept, in order, then its own event. Everything shares the one
//! POST budget: a hub that is down costs a hook no more than before.
//!
//! `cctg hook PermissionRequest` is the one hook that waits: it asks the hub
//! for an answer from Telegram ([`crate::wire::PERMISSION_PATH`]) and prints
//! Claude Code's decision JSON on stdout when there is one. No answer (the
//! channel relays the request, time ran out, the hub is down) prints nothing:
//! Claude Code then shows its own dialog as usual. It is never kept in the
//! spool: an answer that comes late is worthless.
//!
//! `cctg hook ToolStatus` is registered as an `async` hook on `PreToolUse`,
//! `PostToolUse` and `PostToolUseFailure` (TASK-029): Claude Code does not
//! wait for it. It tells the hub which tool call of the main conversation
//! runs now, for the status message; calls inside subagents are skipped.
//!
//! Registration: `docs/hook-settings.json`. Configuration: [`crate::device`].

use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

use crate::device::{self, DeviceConfig};
use crate::proctree::{self, Lineage};
use crate::spool;
use crate::tls::HubAddr;
use crate::wire::{
    Behavior, HOOK_PATH, HookEvent, HookPost, PERMISSION_PATH, PING_PATH, PermissionAnswer,
    PermissionPost, Secret, VERSION,
};

/// Budget of the POST, connect included. `SessionEnd` hooks share 1.5 s in
/// all, start-up and the process snapshot included; `UserPromptSubmit` and the
/// handback matcher block Claude Code while they run. On Windows a connect to
/// a closed local port is retried and lasts until this timeout, so a stopped
/// hub costs every hook exactly this much.
pub const POST_TIMEOUT: Duration = Duration::from_millis(500);
/// `UserPromptSubmit` holds the user's prompt and carries nothing but its id:
/// a shorter wait there. Events with a report or lifecycle keep
/// [`POST_TIMEOUT`], which also leaves room for a hub across Tailscale.
pub const PROMPT_POST_TIMEOUT: Duration = Duration::from_millis(300);
/// [`POST_TIMEOUT`] and [`PROMPT_POST_TIMEOUT`] to a hub over TLS: the
/// handshake adds a round trip to a hub that is not on this machine. Still
/// well under the 1.5 s `SessionEnd` budget.
pub const TLS_POST_TIMEOUT: Duration = Duration::from_millis(900);
pub const TLS_PROMPT_POST_TIMEOUT: Duration = Duration::from_millis(600);
/// Claude Code writes the whole input at once and closes stdin.
pub const STDIN_TIMEOUT: Duration = Duration::from_millis(300);
/// Larger input is dropped, not truncated: cut JSON is not JSON.
pub const MAX_STDIN: u64 = 8 << 20;
/// Cap of one free-text field (assistant message, handback report), in bytes.
/// Even fully `\u`-escaped it keeps the body under `wire::MAX_HOOK_BODY`.
pub const MAX_TEXT: usize = 128 << 10;
const HANDBACK_TOOL: &str = "SubagentHandback";
pub const PERMISSION_EVENT: &str = "PermissionRequest";
/// The command-line name of the tool status hook (see the module docs).
pub const TOOL_STATUS_EVENT: &str = "ToolStatus";
/// Cap of a tool call line, in bytes: the status message shows one line.
const MAX_CALL_LINE: usize = 512;
/// Connect and send of the permission request. On Windows a connect to a
/// closed local port lasts until the timeout, so a stopped hub costs this.
pub const PERMISSION_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// [`PERMISSION_CONNECT_TIMEOUT`] over TLS.
pub const TLS_PERMISSION_CONNECT_TIMEOUT: Duration = Duration::from_millis(1000);
/// Longest wait for the hub's answer, under the `"timeout": 100` the
/// settings give this hook; the hub itself gives up after 90 s.
pub const PERMISSION_WAIT: Duration = Duration::from_secs(97);
/// Cap of the description and of the input preview, in bytes: Telegram
/// shows at most 4096 characters of the prompt anyway.
pub const MAX_PREVIEW: usize = 4 << 10;
const MAX_TOOL_NAME: usize = 256;
/// Longest hub answer read.
const MAX_ANSWER: u64 = 4096;
/// Told to Claude with a refusal.
pub const DENY_MESSAGE: &str = "Denied by the user in Telegram";

/// Runs one hook invocation. Never fails: every problem ends as one fixed
/// line on stderr.
pub async fn run(event: &str) {
    let Some(input) = read_stdin(MAX_STDIN, STDIN_TIMEOUT) else {
        warn!("hook input unreadable, too large or late; nothing sent");
        return;
    };
    let config = DeviceConfig::load();
    if event == PERMISSION_EVENT {
        return permission(&input, &config).await;
    }
    let hook_post = match build_here(event, &input, &config.host) {
        Ok(hook_post) => hook_post,
        Err(skip) => {
            debug!(reason = skip.0, "hook event skipped");
            return;
        }
    };
    let secret = match &config.secret {
        Ok(secret) => secret,
        Err(problem) => {
            warn!(%problem, "hook event not sent");
            return;
        }
    };
    let hub = match config.hub(&config.hook_addr) {
        Ok(hub) => hub,
        Err(problem) => {
            warn!(%problem, "hook event not sent");
            return;
        }
    };
    let spool = config.state_dir.as_deref().map(spool::dir);
    let timeout = post_timeout(&hook_post.event, hub.is_tls());
    let Err(error) = deliver(spool.as_deref(), &hub, secret, &hook_post, timeout).await else {
        return;
    };
    let kept = match spool {
        Some(root) => spool::save(&root, &hook_post, SystemTime::now()),
        None if !spool::keeps(&hook_post.event) => Err(spool::SpoolError::NotKept),
        None => Err(spool::SpoolError::Io(std::io::ErrorKind::NotFound)),
    };
    match kept {
        Ok(()) => {
            warn!(event = hook_post.event.kind(), %error, "hook event not delivered; kept for the next hook")
        }
        Err(spool::SpoolError::NotKept) => {
            warn!(event = hook_post.event.kind(), %error, "hook event not delivered");
        }
        Err(problem) => warn!(
            event = hook_post.event.kind(),
            %error,
            %problem,
            "hook event not delivered and not kept"
        ),
    }
}

/// The `PermissionRequest` hook: asks the hub and prints the decision, if
/// any. Every failure ends quietly with no decision.
async fn permission(input: &[u8], config: &DeviceConfig) {
    let post = match build_permission(input, &config.host) {
        Ok(post) => post,
        Err(skip) => {
            debug!(reason = skip.0, "hook event skipped");
            return;
        }
    };
    let secret = match &config.secret {
        Ok(secret) => secret,
        Err(problem) => {
            warn!(%problem, "permission request not sent");
            return;
        }
    };
    let hub = match config.hub(&config.hook_addr) {
        Ok(hub) => hub,
        Err(problem) => {
            warn!(%problem, "permission request not sent");
            return;
        }
    };
    let connect = if hub.is_tls() {
        TLS_PERMISSION_CONNECT_TIMEOUT
    } else {
        PERMISSION_CONNECT_TIMEOUT
    };
    let asked = ask(&hub, secret, &post, connect, PERMISSION_WAIT).await;
    match asked {
        Ok(Some(behavior)) => {
            let mut stdout = std::io::stdout().lock();
            let _ = std::io::Write::write_all(&mut stdout, decision_json(behavior).as_bytes());
            let _ = std::io::Write::flush(&mut stdout);
        }
        Ok(None) => debug!("no decision from Telegram"),
        Err(error) => warn!(%error, "permission request got no answer from the hub"),
    }
}

/// Claude Code's `PermissionRequest` decision for stdout.
pub fn decision_json(behavior: Behavior) -> String {
    let decision = match behavior {
        Behavior::Allow => json!({ "behavior": "allow" }),
        Behavior::Deny => json!({ "behavior": "deny", "message": DENY_MESSAGE }),
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": PERMISSION_EVENT,
            "decision": decision,
        }
    })
    .to_string()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PermissionInput {
    session_id: String,
    hook_event_name: Option<String>,
    tool_name: String,
    tool_input: Option<Value>,
}

/// Turns a `PermissionRequest` hook input into the hub request: the tool,
/// its `description` when the input has one, and the rest of the input as
/// compact JSON, both capped at [`MAX_PREVIEW`].
pub fn build_permission(input: &[u8], host: &str) -> Result<PermissionPost, Skip> {
    let input: PermissionInput =
        serde_json::from_slice(input).map_err(|_| Skip("input is not a hook JSON object"))?;
    if input.session_id.is_empty() {
        return Err(Skip("input has no session_id"));
    }
    if input
        .hook_event_name
        .as_deref()
        .is_some_and(|name| name != PERMISSION_EVENT)
    {
        return Err(Skip("input is for another hook event"));
    }
    if input.tool_name.trim().is_empty() {
        return Err(Skip("permission request without a tool name"));
    }
    let mut tool_input = input.tool_input.unwrap_or(Value::Null);
    let description = tool_input
        .as_object_mut()
        .and_then(|fields| fields.remove("description"))
        .and_then(|description| description.as_str().map(str::to_owned))
        .map(|description| cap_to(description, MAX_PREVIEW))
        .unwrap_or_default();
    let input_preview = match tool_input {
        Value::Null => String::new(),
        Value::String(text) => cap_to(text, MAX_PREVIEW),
        other => cap_to(other.to_string(), MAX_PREVIEW),
    };
    Ok(PermissionPost {
        v: VERSION,
        host: host.to_owned(),
        session_id: input.session_id,
        tool_name: cap_to(input.tool_name, MAX_TOOL_NAME),
        description,
        input_preview,
    })
}

/// Asks the hub at `addr` for the answer to `post`: connect (and the TLS
/// handshake) and send within `connect_timeout`, then wait up to `wait`.
/// `Ok(None)`: the hub has no decision.
pub async fn ask(
    addr: &HubAddr,
    secret: &Secret,
    post: &PermissionPost,
    connect_timeout: Duration,
    wait: Duration,
) -> Result<Option<Behavior>, PostError> {
    let body = serde_json::to_vec(post).expect("permission posts always serialize");
    let head = format!(
        "POST {PERMISSION_PATH} HTTP/1.1\r\nHost: cctg-hub\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        secret.expose(),
        body.len()
    );
    let io = |error: std::io::Error| PostError::Io(error.kind());
    let send = async {
        let mut stream = addr.connect().await.map_err(io)?;
        stream
            .write_all(&[head.as_bytes(), &body].concat())
            .await
            .map_err(io)?;
        // Over TLS the tail can still sit in the session: push it out
        // before waiting for the answer (reading never does).
        stream.flush().await.map_err(io)?;
        Ok(stream)
    };
    let mut stream = tokio::time::timeout(connect_timeout, send)
        .await
        .unwrap_or(Err(PostError::Timeout(connect_timeout)))?;
    let mut answer = Vec::new();
    tokio::time::timeout(
        wait,
        (&mut stream).take(MAX_ANSWER).read_to_end(&mut answer),
    )
    .await
    .map_err(|_| PostError::Timeout(wait))?
    .map_err(io)?;
    parse_answer(&answer)
}

/// `204`: no decision; `200` with a [`PermissionAnswer`] body: the decision.
fn parse_answer(answer: &[u8]) -> Result<Option<Behavior>, PostError> {
    let line_end = answer
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .ok_or(PostError::BadResponse)?
        + 2;
    match parse_status(&answer[..line_end]) {
        Some(204) => Ok(None),
        Some(200) => {
            let body = answer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .ok_or(PostError::BadResponse)?
                + 4;
            let parsed: PermissionAnswer =
                serde_json::from_slice(&answer[body..]).map_err(|_| PostError::BadResponse)?;
            Ok(Some(parsed.behavior))
        }
        Some(code) => Err(PostError::Status(code)),
        None => Err(PostError::BadResponse),
    }
}

/// Sends what the session kept in `spool` (when there is one), then `post`,
/// all within `timeout`. An error means `post` did not reach the hub: either
/// it failed itself or a kept event before it did (then it was not tried,
/// so the order holds).
async fn deliver(
    spool: Option<&Path>,
    addr: &HubAddr,
    secret: &Secret,
    hook_post: &HookPost,
    timeout: Duration,
) -> Result<(), PostError> {
    let deadline = tokio::time::Instant::now() + timeout;
    if let Some(root) = spool {
        let sent = spool::replay(root, &hook_post.session_id, addr, secret, deadline).await?;
        if sent > 0 {
            debug!(sent, "kept hook events delivered");
        }
    }
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    if left.is_zero() {
        return Err(PostError::Timeout(timeout));
    }
    post(addr, secret, hook_post, left).await
}

/// The POST budget of `event`; `tls`: the hub is reached over TLS.
pub fn post_timeout(event: &HookEvent, tls: bool) -> Duration {
    let short = matches!(
        event,
        HookEvent::UserPromptSubmit { .. }
            | HookEvent::ToolStart { .. }
            | HookEvent::ToolEnd { .. }
            | HookEvent::StatusLine { .. }
    );
    match (short, tls) {
        (true, false) => PROMPT_POST_TIMEOUT,
        (false, false) => POST_TIMEOUT,
        (true, true) => TLS_PROMPT_POST_TIMEOUT,
        (false, true) => TLS_POST_TIMEOUT,
    }
}

/// [`build`] with this device as the probe.
fn build_here(event: &str, input: &[u8], host: &str) -> Result<HookPost, Skip> {
    let env_pid = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|pid| pid.trim().parse().ok());
    let env_session = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let lineage =
        |session: &str| proctree::current_lineage(env_pid, env_session.as_deref(), session);
    let probe = Probe {
        host,
        cwd: &device::canonical_cwd,
        lineage: &lineage,
        live_pids: &proctree::live_claude_pids,
        exists: &|path| path.exists(),
    };
    build(event, input, &probe)
}

/// Reads stdin on its own thread: a blocking read cannot be cancelled, and a
/// tokio stdin read would hold up runtime shutdown. The caller exits the
/// process, which ends the thread if it is still blocked.
pub(crate) fn read_stdin(limit: u64, timeout: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = std::io::stdin()
            .lock()
            .take(limit + 1)
            .read_to_end(&mut buf);
        let _ = tx.send(read.ok().map(|_| buf));
    });
    let buf = rx.recv_timeout(timeout).ok()??;
    (buf.len() as u64 <= limit).then_some(buf)
}

/// What a hook learns from its device besides stdin. Injected for tests.
pub struct Probe<'a> {
    pub host: &'a str,
    /// Folder resolution, see [`device::canonical_cwd`].
    pub cwd: &'a dyn Fn(&str) -> String,
    /// Process-tree lineage for the stdin session id; asked only by
    /// `SessionStart` and `SessionEnd`.
    pub lineage: &'a dyn Fn(&str) -> Lineage,
    /// Claude processes alive on this device, see
    /// [`proctree::live_claude_pids`]; asked only by `SessionStart` and
    /// `SessionEnd`.
    pub live_pids: &'a dyn Fn() -> Option<Vec<u32>>,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

/// Why an input produced no POST. Fixed text, safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skip(pub &'static str);

/// The hook input fields cctg uses; everything else is ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Input {
    session_id: String,
    cwd: String,
    transcript_path: String,
    hook_event_name: Option<String>,
    source: Option<String>,
    reason: Option<String>,
    prompt_id: Option<String>,
    last_assistant_message: Option<String>,
    agent_id: Option<String>,
    agent_type: Option<String>,
    agent_transcript_path: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<ToolInput>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolInput {
    message: Option<String>,
}

/// The fields of a `PreToolUse` / `PostToolUse` / `PostToolUseFailure` input
/// the tool status uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolStatusInput {
    session_id: String,
    cwd: String,
    transcript_path: String,
    hook_event_name: Option<String>,
    tool_name: String,
    tool_use_id: String,
    tool_input: Option<Value>,
    agent_id: Option<String>,
}

/// `cctg hook ToolStatus`: the start or the end of a tool call of the main
/// conversation. Handback calls (their own hook) and every call inside a
/// subagent are skipped before any POST.
fn build_tool_status(input: &[u8], probe: &Probe<'_>) -> Result<HookPost, Skip> {
    let input: ToolStatusInput =
        serde_json::from_slice(input).map_err(|_| Skip("input is not a hook JSON object"))?;
    if input.session_id.is_empty() {
        return Err(Skip("input has no session_id"));
    }
    if input.agent_id.is_some_and(|id| !id.is_empty()) {
        return Err(Skip("tool call inside a subagent"));
    }
    if input.tool_use_id.is_empty() || input.tool_name.trim().is_empty() {
        return Err(Skip("tool event without its call"));
    }
    if input.tool_name == HANDBACK_TOOL {
        return Err(Skip("handback has its own hook"));
    }
    let hook_event = match input.hook_event_name.as_deref() {
        Some("PreToolUse") => {
            let tool_input = input.tool_input.unwrap_or(Value::Null);
            HookEvent::ToolStart {
                line: cap_to(
                    transcript::call_line(&input.tool_name, &tool_input),
                    MAX_CALL_LINE,
                ),
                tool_use_id: cap_to(input.tool_use_id, MAX_TOOL_NAME),
            }
        }
        Some("PostToolUse" | "PostToolUseFailure") => HookEvent::ToolEnd {
            tool_use_id: cap_to(input.tool_use_id, MAX_TOOL_NAME),
        },
        _ => return Err(Skip("input is for another hook event")),
    };
    Ok(HookPost::new(
        probe.host.to_owned(),
        input.session_id,
        (probe.cwd)(&input.cwd),
        input.transcript_path,
        hook_event,
    ))
}

/// Turns one hook input into the POST for the hub, or says why there is none.
/// `event` is the name the settings passed on the command line.
pub fn build(event: &str, input: &[u8], probe: &Probe<'_>) -> Result<HookPost, Skip> {
    if event == TOOL_STATUS_EVENT {
        return build_tool_status(input, probe);
    }
    let input: Input =
        serde_json::from_slice(input).map_err(|_| Skip("input is not a hook JSON object"))?;
    if input.session_id.is_empty() {
        return Err(Skip("input has no session_id"));
    }
    if input
        .hook_event_name
        .as_deref()
        .is_some_and(|name| name != event)
    {
        return Err(Skip("input is for another hook event"));
    }
    let hook_event = match event {
        "SessionStart" => {
            let lineage = (probe.lineage)(&input.session_id);
            HookEvent::SessionStart {
                source: input.source,
                claude_pid: lineage.claude_pid,
                parent_claude_pid: lineage.parent_claude_pid,
            }
        }
        "SessionEnd" => HookEvent::SessionEnd {
            reason: input.reason,
            claude_pid: (probe.lineage)(&input.session_id).claude_pid,
        },
        "UserPromptSubmit" => HookEvent::UserPromptSubmit {
            prompt_id: input.prompt_id,
        },
        "Stop" => HookEvent::Stop {
            prompt_id: input.prompt_id,
            last_assistant_message: input.last_assistant_message.map(cap_text),
        },
        "SubagentStart" => {
            // Start payloads observed in TASK-003 have no
            // `agent_transcript_path`, and real subagent files may not exist
            // yet. The empty/blank type signal is therefore the only safe
            // internal-agent filter here. A typed `--agent` start cannot be
            // distinguished without a future upstream field.
            HookEvent::SubagentStart {
                agent_id: agent_id(input.agent_id)?,
                agent_type: agent_type(input.agent_type)?,
            }
        }
        "SubagentStop" => {
            let agent_id = agent_id(input.agent_id)?;
            let agent_type = agent_type(input.agent_type)?;
            // Claude Code's own agents (prompt suggestions, /btw, the
            // `--agent` session name) leave no subagent files behind; a stop
            // that names no subagent transcript cannot be told apart from them.
            let agent_transcript_path = input
                .agent_transcript_path
                .filter(|path| !path.is_empty() && has_agent_files(path, probe.exists))
                .ok_or(Skip("internal agent"))?;
            HookEvent::SubagentStop {
                agent_id,
                agent_type,
                agent_transcript_path: Some(agent_transcript_path),
                last_assistant_message: input.last_assistant_message.map(cap_text),
            }
        }
        "PreToolUse" | "PostToolUse" => {
            if input.tool_name.as_deref() != Some(HANDBACK_TOOL) {
                return Err(Skip("tool is not SubagentHandback"));
            }
            let message = input
                .tool_input
                .and_then(|tool_input| tool_input.message)
                .ok_or(Skip("handback without a message"))?;
            HookEvent::SubagentHandback {
                agent_id: agent_id(input.agent_id)?,
                message: cap_text(message),
            }
        }
        _ => return Err(Skip("unsupported hook event")),
    };
    let live = matches!(
        hook_event,
        HookEvent::SessionStart { .. } | HookEvent::SessionEnd { .. }
    );
    let mut post = HookPost::new(
        probe.host.to_owned(),
        input.session_id,
        (probe.cwd)(&input.cwd),
        input.transcript_path,
        hook_event,
    );
    if live {
        post.live_claude_pids = (probe.live_pids)();
    }
    Ok(post)
}

fn agent_id(id: Option<String>) -> Result<String, Skip> {
    id.filter(|id| !id.is_empty())
        .ok_or(Skip("subagent event without agent_id"))
}

/// An empty type is a Claude Code internal agent (TASK-003: 14 of 14).
fn agent_type(kind: Option<String>) -> Result<String, Skip> {
    kind.filter(|kind| !kind.trim().is_empty())
        .ok_or(Skip("internal agent"))
}

/// A real subagent leaves `agent-<id>.jsonl` and/or `agent-<id>.meta.json`;
/// the 14 internal agents of TASK-003 left neither.
fn has_agent_files(transcript: &str, exists: &dyn Fn(&Path) -> bool) -> bool {
    let meta = transcript
        .strip_suffix(".jsonl")
        .map(|stem| format!("{stem}.meta.json"));
    exists(Path::new(transcript)) || meta.is_some_and(|meta| exists(Path::new(&meta)))
}

fn cap_text(text: String) -> String {
    cap_to(text, MAX_TEXT)
}

fn cap_to(mut text: String, max: usize) -> String {
    if text.len() > max {
        let cut = text.floor_char_boundary(max - '\u{2026}'.len_utf8());
        text.truncate(cut);
        text.push('\u{2026}');
    }
    text
}

const MAX_STATUS_LINE: u64 = 256;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error("hub did not answer within {0:?}")]
    Timeout(Duration),
    #[error("cannot reach the hub: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("hub answered HTTP {0}")]
    Status(u16),
    #[error("hub answer is not HTTP")]
    BadResponse,
}

/// Sends `post` to the hub hook endpoint at `addr`. Everything, connect and
/// TLS handshake included, fits in `timeout`. `Ok` means the hub has the
/// event.
pub async fn post(
    addr: &HubAddr,
    secret: &Secret,
    post: &HookPost,
    timeout: Duration,
) -> Result<(), PostError> {
    let body = serde_json::to_vec(post).expect("hook posts always serialize");
    match exchange(addr, HOOK_PATH, secret, &body, timeout).await? {
        204 => Ok(()),
        code => Err(PostError::Status(code)),
    }
}

/// `POST /v1/ping` (TASK-031, `cctg doctor`): `Ok` when the hub took the
/// secret. A wrong secret gets 401; a hub older than the route answers 404
/// before it looks at the secret.
pub async fn ping(addr: &HubAddr, secret: &Secret, timeout: Duration) -> Result<(), PostError> {
    match exchange(addr, PING_PATH, secret, &[], timeout).await? {
        204 => Ok(()),
        code => Err(PostError::Status(code)),
    }
}

/// One request to the hub hook endpoint within `timeout`; the answer's
/// status code.
async fn exchange(
    addr: &HubAddr,
    path: &str,
    secret: &Secret,
    body: &[u8],
    timeout: Duration,
) -> Result<u16, PostError> {
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: cctg-hub\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        secret.expose(),
        body.len()
    );
    let exchange = async {
        let io = |error: std::io::Error| PostError::Io(error.kind());
        let mut stream = addr.connect().await.map_err(io)?;
        stream
            .write_all(&[head.as_bytes(), body].concat())
            .await
            .map_err(io)?;
        // Over TLS the tail can still sit in the session: push it out
        // before waiting for the answer (reading never does).
        stream.flush().await.map_err(io)?;
        let mut status_line = Vec::new();
        BufReader::new(stream)
            .take(MAX_STATUS_LINE)
            .read_until(b'\n', &mut status_line)
            .await
            .map_err(io)?;
        parse_status(&status_line).ok_or(PostError::BadResponse)
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .unwrap_or(Err(PostError::Timeout(timeout)))
}

/// Accepts only a complete `HTTP/1.1 NNN[ reason]\r\n` line: a truncated or
/// foreign answer must never count as delivered.
fn parse_status(line: &[u8]) -> Option<u16> {
    let rest = line.strip_suffix(b"\r\n")?.strip_prefix(b"HTTP/1.1 ")?;
    let (code, tail) = rest.split_at_checked(3)?;
    if !code.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {
        return None;
    }
    std::str::from_utf8(code).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::hub::ingress;
    use crate::wire::HookEvent;

    const SECRET: &str = "0123456789abcdef-secret";

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    fn sample() -> HookPost {
        HookPost::new(
            "box".into(),
            "5e551017-0000-4000-8000-000000000001".into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: None,
            },
        )
    }

    async fn hub() -> (String, mpsc::Receiver<HookPost>) {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(ingress::serve_hooks(listener, secret(), tx));
        (addr, rx)
    }

    #[tokio::test]
    async fn a_resent_post_reaches_the_hub_once() {
        let (addr, mut events) = hub().await;
        let sent = sample();
        let timeout = Duration::from_secs(5);
        let started = Instant::now();
        post(&HubAddr::plain(addr.as_str()), &secret(), &sent, timeout)
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        post(&HubAddr::plain(addr.as_str()), &secret(), &sent, timeout)
            .await
            .unwrap();
        assert_eq!(events.recv().await, Some(sent));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_wrong_secret_is_refused() {
        let (addr, mut events) = hub().await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        let result = post(
            &HubAddr::plain(addr.as_str()),
            &wrong,
            &sample(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(result, Err(PostError::Status(401)));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_silent_hub_costs_at_most_the_timeout() {
        // Accepts and never answers.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let _held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream);
            }
        });
        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let result = post(
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &sample(),
            timeout,
        )
        .await;
        assert_eq!(result, Err(PostError::Timeout(timeout)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn no_hub_is_an_error_within_the_timeout() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let timeout = Duration::from_millis(800);
        let started = Instant::now();
        let error = post(
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &sample(),
            timeout,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, PostError::Io(_) | PostError::Timeout(_)),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(!format!("{error} {error:?}").contains(SECRET));
    }

    fn kept_start() -> HookPost {
        HookPost::new(
            "box".into(),
            sample().session_id,
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(7),
                parent_claude_pid: None,
            },
        )
    }

    #[tokio::test]
    async fn kept_events_of_the_session_go_before_its_own() {
        let dir = crate::hub::testdir::TempDir::new("hook-deliver");
        let root = dir.path().join("spool");
        let kept = kept_start();
        spool::save(&root, &kept, SystemTime::now()).unwrap();
        let (addr, mut events) = hub().await;
        let own = sample();
        let timeout = Duration::from_secs(5);
        deliver(
            Some(&root),
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &own,
            timeout,
        )
        .await
        .unwrap();
        assert_eq!(events.recv().await, Some(kept));
        assert_eq!(events.recv().await, Some(own));
        assert!(spool::pending(&root, &sample().session_id, SystemTime::now()).is_empty());
    }

    #[tokio::test]
    async fn a_failed_kept_event_stops_the_hook_within_its_budget() {
        let dir = crate::hub::testdir::TempDir::new("hook-deliver-fail");
        let root = dir.path().join("spool");
        spool::save(&root, &kept_start(), SystemTime::now()).unwrap();
        // Accepts, counts and never answers.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = accepted.clone();
        let _held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                open.push(stream);
            }
        });
        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let result = deliver(
            Some(&root),
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &sample(),
            timeout,
        )
        .await;
        assert!(matches!(result, Err(PostError::Timeout(_))), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        // The own event was never tried: it would overtake the kept start.
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            spool::pending(&root, &sample().session_id, SystemTime::now()).len(),
            1
        );
    }

    #[tokio::test]
    async fn a_refused_kept_event_keeps_the_own_event_back() {
        let dir = crate::hub::testdir::TempDir::new("hook-deliver-refused");
        let root = dir.path().join("spool");
        spool::save(&root, &kept_start(), SystemTime::now()).unwrap();
        // Answers every request 503 at once and counts them.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 64 * 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = stream.shutdown().await;
            }
        });
        let result = deliver(
            Some(&root),
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &sample(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(result, Err(PostError::Status(503)));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Time was left, yet the own event was not sent past the kept start.
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn permission_post() -> PermissionPost {
        PermissionPost {
            v: VERSION,
            host: "box".into(),
            session_id: sample().session_id,
            tool_name: "Bash".into(),
            description: String::new(),
            input_preview: String::new(),
        }
    }

    /// A hub whose asks are answered by `decide` (`None`: dropped).
    async fn permission_hub(
        decide: impl Fn(&PermissionPost) -> Option<Option<Behavior>> + Send + 'static,
    ) -> String {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (events, _events_rx) = mpsc::channel(8);
        let (asks, mut asks_rx) = mpsc::channel::<ingress::PermissionAsk>(8);
        tokio::spawn(async move {
            let _events_rx = _events_rx;
            while let Some(ask) = asks_rx.recv().await {
                if let Some(answer) = decide(&ask.post) {
                    let _ = ask.answer.send(answer);
                }
            }
        });
        tokio::spawn(ingress::serve_hooks_and_permissions(
            listener,
            secret(),
            events,
            asks,
        ));
        addr
    }

    #[tokio::test]
    async fn a_permission_ask_returns_the_hub_decision_or_none() {
        let wait = Duration::from_secs(5);
        let connect = Duration::from_secs(2);
        for (decision, want) in [
            (Some(Some(Behavior::Allow)), Some(Behavior::Allow)),
            (Some(Some(Behavior::Deny)), Some(Behavior::Deny)),
            (Some(None), None),
            (None, None),
        ] {
            let addr = permission_hub(move |post| {
                assert_eq!(post.tool_name, "Bash");
                decision
            })
            .await;
            let got = ask(
                &HubAddr::plain(addr.as_str()),
                &secret(),
                &permission_post(),
                connect,
                wait,
            )
            .await;
            assert_eq!(got, Ok(want), "{decision:?}");
        }
        // A wrong secret is refused before the hub sees the ask.
        let addr = permission_hub(|_| panic!("asked without the secret")).await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        let got = ask(
            &HubAddr::plain(addr.as_str()),
            &wrong,
            &permission_post(),
            connect,
            wait,
        )
        .await;
        assert_eq!(got, Err(PostError::Status(401)));
    }

    #[tokio::test]
    async fn a_hub_without_the_permission_path_means_no_decision() {
        let (addr, _events) = hub().await;
        let got = ask(
            &HubAddr::plain(addr.as_str()),
            &secret(),
            &permission_post(),
            Duration::from_secs(2),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(got, Err(PostError::Status(404)));
    }

    #[test]
    fn permission_answers_parse_strictly() {
        assert_eq!(
            parse_answer(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"),
            Ok(None)
        );
        assert_eq!(
            parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"behavior\":\"allow\"}"),
            Ok(Some(Behavior::Allow))
        );
        for bad in [
            &b"HTTP/1.1 200 OK\r\n\r\n{\"behavior\":\"maybe\"}"[..],
            b"HTTP/1.1 200 OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n",
            b"HTTP/1.1 204",
            b"",
        ] {
            assert_eq!(
                parse_answer(bad),
                Err(PostError::BadResponse),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(
            parse_answer(b"HTTP/1.1 503 Service Unavailable\r\n\r\n"),
            Err(PostError::Status(503))
        );
    }

    #[test]
    fn status_lines() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_status(b"HTTP/1.1 401 Unauthorized\r\n"), Some(401));
        assert_eq!(parse_status(b"HTTP/1.1 204\r\n"), Some(204));
        for bad in [
            &b"HTTP/1.1 204 No Content"[..],
            b"HTTP/1.1 204",
            b"HTTP/1.1 204 No Content\n",
            b"HTTP/1.x 204 No Content\r\n",
            b"HTTP/1.0 204 No Content\r\n",
            b"HTTP/1.1 20 No Content\r\n",
            b"HTTP/1.1 2044 No Content\r\n",
            b"HTTP/1.1 2x4 No Content\r\n",
            b"HTTP/1.1  204 No Content\r\n",
            b"SSH-2.0-x\r\n",
            b"",
        ] {
            assert_eq!(parse_status(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    /// A fake hub that reads the request, writes `answer` and closes.
    async fn answering(answer: &'static [u8]) -> String {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = vec![0u8; 64 * 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(answer).await;
                let _ = stream.shutdown().await;
                let _ = stream.read_to_end(&mut request).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_truncated_or_foreign_status_line_is_not_success() {
        let timeout = Duration::from_secs(5);
        for answer in [
            &b"HTTP/1.1 204"[..],
            b"HTTP/1.x 204 No Content\r\n\r\n",
            b"HTTP/1.1 2040 No Content\r\n\r\n",
            b"",
        ] {
            let addr = answering(answer).await;
            let result = post(
                &HubAddr::plain(addr.as_str()),
                &secret(),
                &sample(),
                timeout,
            )
            .await;
            assert_eq!(
                result,
                Err(PostError::BadResponse),
                "{}",
                String::from_utf8_lossy(answer)
            );
        }
        let addr = answering(b"HTTP/1.1 204 No Content\r\n\r\n").await;
        assert_eq!(
            post(
                &HubAddr::plain(addr.as_str()),
                &secret(),
                &sample(),
                timeout
            )
            .await,
            Ok(())
        );
    }

    /// Over TLS the request must leave the TLS buffer before the answer is
    /// awaited: `write_all` can return with the tail still in the TLS
    /// session when the socket is full, and reading never pushes it out. A
    /// hub that has not read yet (a slow link, a busy hub) must still get
    /// every byte (TASK-035 review). The sizes straddle what the socket
    /// buffers of both ends hold before a send blocks.
    #[tokio::test]
    async fn a_post_over_tls_is_whole_even_when_the_hub_reads_late() {
        use rustls::pki_types::pem::PemObject;
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(
            signing_key.serialize_pem().as_bytes(),
        )
        .unwrap();
        let (acceptor, pin) = crate::tls::Acceptor::new(vec![cert.der().clone()], key).unwrap();
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        socket
            .bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .unwrap();
        let listener = socket.listen(8).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(tcp).await else {
                        return;
                    };
                    // Nothing is read for a while: the client's sends block.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let mut buf = Vec::new();
                    let mut chunk = vec![0u8; 64 * 1024];
                    let mut want = usize::MAX;
                    while buf.len() < want {
                        let Ok(n) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if want == usize::MAX
                            && let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                            let length: usize = head
                                .lines()
                                .find_map(|line| line.strip_prefix("Content-Length: "))
                                .and_then(|value| value.parse().ok())
                                .unwrap_or(0);
                            want = end + 4 + length;
                        }
                    }
                    let _ = stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        let hub = HubAddr::pinned(&addr, pin).unwrap();
        for kib in (64..=512).step_by(64) {
            let mut large = sample();
            large.event = HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: Some("x".repeat(kib * 1024)),
            };
            let result = post(&hub, &secret(), &large, Duration::from_secs(3)).await;
            assert_eq!(result, Ok(()), "{kib} KiB");
        }
    }
}

#[cfg(test)]
mod build_tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_json::Value;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hook")
            .join(format!("{name}.json"));
        std::fs::read(&path).expect("hook fixture")
    }

    const OWN: Lineage = Lineage {
        claude_pid: Some(28764),
        parent_claude_pid: None,
    };

    /// `exists` answers true for every path: subagent files present.
    fn with_probe<T>(
        lineage: Lineage,
        exists: bool,
        f: impl FnOnce(&Probe<'_>, &Cell<u32>) -> T,
    ) -> T {
        let asked = Cell::new(0);
        let lineage_fn = |_: &str| {
            asked.set(asked.get() + 1);
            lineage
        };
        let cwd_fn = |cwd: &str| format!("canon:{cwd}");
        let exists_fn = move |_: &Path| exists;
        let probe = Probe {
            host: "box",
            cwd: &cwd_fn,
            lineage: &lineage_fn,
            live_pids: &|| None,
            exists: &exists_fn,
        };
        f(&probe, &asked)
    }

    fn keys(value: &Value) -> BTreeSet<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Serialized POST: the top-level keys are always the same, and the
    /// event object has exactly `want` keys.
    fn check(event: &str, input: &[u8], lineage: Lineage, want: &[&str]) -> Value {
        with_probe(lineage, true, |probe, asked| {
            let post = build(event, input, probe).expect("event is sent");
            let value = serde_json::to_value(&post).unwrap();
            assert_eq!(
                keys(&value),
                set(&[
                    "v",
                    "event_id",
                    "host",
                    "session_id",
                    "cwd",
                    "transcript_path",
                    "event",
                    "client_version"
                ])
            );
            assert_eq!(keys(&value["event"]), set(want), "{event}");
            let walks = u32::from(matches!(event, "SessionStart" | "SessionEnd"));
            assert_eq!(asked.get(), walks, "{event}: process tree asked");
            assert_eq!(value["host"], "box");
            assert!(value["cwd"].as_str().unwrap().starts_with("canon:"));
            value
        })
    }

    #[test]
    fn each_event_carries_its_fields_and_nothing_else() {
        let v = check(
            "SessionStart",
            &fixture("session_start"),
            OWN,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["source"], "startup");
        assert_eq!(v["event"]["claude_pid"], 28764);
        assert_eq!(v["event"]["parent_claude_pid"], Value::Null);
        assert_eq!(v["session_id"], "1e087ca8-58b5-4c0b-9f03-b7926645b744");
        assert_eq!(v["cwd"], "canon:~\\dev\\cctg");
        assert!(
            v["transcript_path"]
                .as_str()
                .unwrap()
                .ends_with("1e087ca8-58b5-4c0b-9f03-b7926645b744.jsonl")
        );

        let v = check(
            "SessionEnd",
            &fixture("session_end"),
            OWN,
            &["type", "reason", "claude_pid"],
        );
        assert_eq!(v["event"]["reason"], "other");
        assert_eq!(v["event"]["claude_pid"], 28764);

        let v = check(
            "UserPromptSubmit",
            &fixture("user_prompt_submit"),
            OWN,
            &["type", "prompt_id"],
        );
        assert_eq!(
            v["event"]["prompt_id"],
            "0bb27012-0d04-43a9-9fa9-128e91ee7e34"
        );
        assert!(!v.to_string().contains("private prompt text"));

        let v = check(
            "Stop",
            &fixture("stop"),
            OWN,
            &["type", "prompt_id", "last_assistant_message"],
        );
        assert_eq!(v["event"]["last_assistant_message"], "Ok.");

        let v = check(
            "SubagentStart",
            &fixture("subagent_start"),
            OWN,
            &["type", "agent_id", "agent_type"],
        );
        assert_eq!(v["event"]["agent_id"], "a8c1bff86acd31609");
        assert_eq!(v["event"]["agent_type"], "Explore");

        let v = check(
            "SubagentStop",
            &fixture("subagent_stop"),
            OWN,
            &[
                "type",
                "agent_id",
                "agent_type",
                "agent_transcript_path",
                "last_assistant_message",
            ],
        );
        assert!(
            v["event"]["agent_transcript_path"]
                .as_str()
                .unwrap()
                .ends_with("agent-a8c1bff86acd31609.jsonl")
        );
        assert!(
            v["event"]["last_assistant_message"]
                .as_str()
                .unwrap()
                .contains("cctg")
        );
        assert!(!v.to_string().contains("background_tasks"));

        for (event, name) in [
            ("PreToolUse", "pre_tool_use_handback"),
            ("PostToolUse", "post_tool_use_handback"),
        ] {
            let v = check(event, &fixture(name), OWN, &["type", "agent_id", "message"]);
            assert_eq!(v["event"]["type"], "subagent_handback");
            assert_eq!(v["event"]["agent_id"], "ae1e99b76211e2536");
            assert_eq!(v["event"]["message"], "2+2 = 4.");
        }
    }

    #[test]
    fn nesting_comes_from_the_lineage() {
        let nested = Lineage {
            claude_pid: Some(25388),
            parent_claude_pid: Some(776),
        };
        let v = check(
            "SessionStart",
            &fixture("session_start"),
            nested,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["claude_pid"], 25388);
        assert_eq!(v["event"]["parent_claude_pid"], 776);
        // SessionEnd reports the own pid only, never the parent.
        let v = check(
            "SessionEnd",
            &fixture("session_end"),
            nested,
            &["type", "reason", "claude_pid"],
        );
        assert_eq!(v["event"]["claude_pid"], 25388);
    }

    #[test]
    fn only_session_start_and_end_carry_the_live_claude_pids() {
        let asked = Cell::new(0);
        let live = || {
            asked.set(asked.get() + 1);
            Some(vec![7, 28764])
        };
        let probe = Probe {
            host: "box",
            cwd: &|cwd: &str| cwd.to_owned(),
            lineage: &|_: &str| OWN,
            live_pids: &live,
            exists: &|_: &Path| true,
        };
        for (event, name) in [
            ("SessionStart", "session_start"),
            ("SessionEnd", "session_end"),
            ("UserPromptSubmit", "user_prompt_submit"),
            ("Stop", "stop"),
            ("SubagentStart", "subagent_start"),
            ("SubagentStop", "subagent_stop"),
            ("PostToolUse", "post_tool_use_handback"),
        ] {
            let post = build(event, &fixture(name), &probe).unwrap();
            let want = matches!(event, "SessionStart" | "SessionEnd").then(|| vec![7, 28764]);
            assert_eq!(post.live_claude_pids, want, "{event}");
            let value = serde_json::to_value(&post).unwrap();
            assert_eq!(
                value.get("live_claude_pids").is_some(),
                want.is_some(),
                "{event}"
            );
        }
        assert_eq!(
            asked.get(),
            2,
            "the process list is read by the two lifecycle hooks only"
        );
    }

    #[test]
    fn source_is_optional_and_read_only_from_session_start() {
        let v = check(
            "SessionStart",
            br#"{"session_id":"s","cwd":"/w","transcript_path":"/t"}"#,
            OWN,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["source"], Value::Null);
        // A `source` on another event is ignored, not an error.
        check(
            "SessionEnd",
            br#"{"session_id":"s","source":"startup","hook_event_name":"SessionEnd"}"#,
            OWN,
            &["type", "reason", "claude_pid"],
        );
    }

    #[test]
    fn internal_agents_are_dropped() {
        with_probe(OWN, true, |probe, _| {
            // Empty agent_type, as in all 14 TASK-003 noise events.
            assert_eq!(
                build("SubagentStop", &fixture("subagent_stop_internal"), probe).unwrap_err(),
                Skip("internal agent")
            );
            for start in [
                &br#"{"session_id":"s","agent_id":"a1","agent_type":""}"#[..],
                br#"{"session_id":"s","agent_id":"a1","agent_type":" \t "}"#,
            ] {
                assert_eq!(
                    build("SubagentStart", start, probe).unwrap_err(),
                    Skip("internal agent")
                );
            }
        });
        // A typed agent (the `--agent` session name) with no subagent files.
        let typed = br#"{"session_id":"s","agent_id":"a1","agent_type":"my-agent","agent_transcript_path":"/p/s/subagents/agent-a1.jsonl"}"#;
        with_probe(OWN, false, |probe, _| {
            assert_eq!(
                build("SubagentStop", typed, probe).unwrap_err(),
                Skip("internal agent")
            );
            // Applying the stop's file-existence rule here would drop real
            // starts because their files are created asynchronously.
            assert!(build("SubagentStart", typed, probe).is_ok());
        });
        // A typed stop without a subagent transcript path at all.
        with_probe(OWN, true, |probe, _| {
            for input in [
                &br#"{"session_id":"s","agent_id":"a1","agent_type":"my-agent"}"#[..],
                br#"{"session_id":"s","agent_id":"a1","agent_type":"my-agent","agent_transcript_path":""}"#,
            ] {
                assert_eq!(
                    build("SubagentStop", input, probe).unwrap_err(),
                    Skip("internal agent")
                );
            }
        });
        // Only the meta file exists yet: a real subagent.
        let asked = std::cell::RefCell::new(Vec::new());
        let exists = |path: &Path| {
            asked.borrow_mut().push(path.to_string_lossy().into_owned());
            path.to_string_lossy().ends_with(".meta.json")
        };
        let probe = Probe {
            host: "box",
            cwd: &|cwd: &str| cwd.to_owned(),
            lineage: &|_: &str| OWN,
            live_pids: &|| None,
            exists: &exists,
        };
        assert!(build("SubagentStop", typed, &probe).is_ok());
        assert!(
            asked
                .borrow()
                .contains(&"/p/s/subagents/agent-a1.meta.json".to_owned())
        );
    }

    fn tool_input(event: &str, tool: &str, extra: serde_json::Value) -> Vec<u8> {
        let mut input = serde_json::json!({
            "session_id": "s",
            "cwd": "/w",
            "transcript_path": "/t/s.jsonl",
            "hook_event_name": event,
            "tool_name": tool,
            "tool_use_id": "toolu_01",
            "tool_input": { "command": "cargo test", "description": "Run tests" },
        });
        if let (Some(target), Some(extra)) = (input.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        input.to_string().into_bytes()
    }

    #[test]
    fn tool_status_carries_the_call_line() {
        let v = check(
            TOOL_STATUS_EVENT,
            &tool_input("PreToolUse", "Bash", serde_json::json!({})),
            OWN,
            &["type", "tool_use_id", "line"],
        );
        assert_eq!(v["event"]["type"], "tool_start");
        assert_eq!(v["event"]["tool_use_id"], "toolu_01");
        assert_eq!(v["event"]["line"], "• Bash: Run tests");
        assert!(!v.to_string().contains("cargo test"), "only the line goes");
        for event in ["PostToolUse", "PostToolUseFailure"] {
            let v = check(
                TOOL_STATUS_EVENT,
                &tool_input(event, "Bash", serde_json::json!({})),
                OWN,
                &["type", "tool_use_id"],
            );
            assert_eq!(v["event"]["type"], "tool_end");
        }
    }

    #[test]
    fn tool_status_skips_subagent_calls_handbacks_and_other_events() {
        with_probe(OWN, true, |probe, asked| {
            for input in [
                tool_input(
                    "PreToolUse",
                    "Bash",
                    serde_json::json!({ "agent_id": "a1" }),
                ),
                tool_input("PreToolUse", HANDBACK_TOOL, serde_json::json!({})),
                tool_input(
                    "PreToolUse",
                    "Bash",
                    serde_json::json!({ "tool_use_id": "" }),
                ),
                tool_input("Stop", "Bash", serde_json::json!({})),
                tool_input(
                    "PreToolUse",
                    "Bash",
                    serde_json::json!({ "session_id": "" }),
                ),
                br#"{"session_id":"s","tool_name":"Bash","tool_use_id":"t"}"#.to_vec(),
                b"not json".to_vec(),
            ] {
                assert!(
                    build(TOOL_STATUS_EVENT, &input, probe).is_err(),
                    "{}",
                    String::from_utf8_lossy(&input)
                );
            }
            assert_eq!(asked.get(), 0, "no process tree walk for tool events");
            // The old hooks keep their meaning: a Bash call is no handback.
            let bash = tool_input("PreToolUse", "Bash", serde_json::json!({}));
            assert!(build("PreToolUse", &bash, probe).is_err());
        });
    }

    #[test]
    fn other_tools_and_incomplete_handbacks_are_skipped() {
        with_probe(OWN, true, |probe, _| {
            let bash = br#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
            assert!(build("PreToolUse", bash, probe).is_err());
            let no_message = br#"{"session_id":"s","agent_id":"a","tool_name":"SubagentHandback","tool_input":{}}"#;
            assert!(build("PostToolUse", no_message, probe).is_err());
            let no_agent = br#"{"session_id":"s","tool_name":"SubagentHandback","tool_input":{"message":"m"}}"#;
            assert!(build("PostToolUse", no_agent, probe).is_err());
        });
    }

    #[test]
    fn broken_input_is_skipped_without_panicking() {
        let mut whole = fixture("session_start");
        while whole.last().is_some_and(u8::is_ascii_whitespace) {
            whole.pop();
        }
        with_probe(OWN, true, |probe, asked| {
            for cut in 0..whole.len() {
                // Every prefix of a real input, plus garbage.
                let _ = build("SessionStart", &whole[..cut], probe);
            }
            for input in [
                &b""[..],
                b"\xff\xfe\x00",
                b"null",
                b"[]",
                b"42",
                br#"{"session_id":7}"#,
                br#"{"session_id":""}"#,
                br#"{"cwd":"/w"}"#,
                br#"{"session_id":"s","tool_input":"x"}"#,
            ] {
                assert!(build("SessionStart", input, probe).is_err());
                assert!(build("PostToolUse", input, probe).is_err());
            }
            assert!(asked.get() <= 1, "tree walked for rejected input");
        });
    }

    #[test]
    fn misrouted_and_unknown_events_are_skipped() {
        with_probe(OWN, true, |probe, _| {
            assert_eq!(
                build("SessionStart", &fixture("session_end"), probe).unwrap_err(),
                Skip("input is for another hook event")
            );
            assert_eq!(
                build("PreCompact", br#"{"session_id":"s"}"#, probe).unwrap_err(),
                Skip("unsupported hook event")
            );
        });
    }

    #[test]
    fn only_the_prompt_hook_uses_the_short_post_timeout() {
        with_probe(OWN, true, |probe, _| {
            for (event, name) in [
                ("SessionStart", "session_start"),
                ("SessionEnd", "session_end"),
                ("UserPromptSubmit", "user_prompt_submit"),
                ("Stop", "stop"),
                ("SubagentStart", "subagent_start"),
                ("SubagentStop", "subagent_stop"),
                ("PostToolUse", "post_tool_use_handback"),
            ] {
                let post = build(event, &fixture(name), probe).unwrap();
                let want = if event == "UserPromptSubmit" {
                    PROMPT_POST_TIMEOUT
                } else {
                    POST_TIMEOUT
                };
                assert_eq!(post_timeout(&post.event, false), want, "{event}");
                let tls = if event == "UserPromptSubmit" {
                    TLS_PROMPT_POST_TIMEOUT
                } else {
                    TLS_POST_TIMEOUT
                };
                assert_eq!(post_timeout(&post.event, true), tls, "{event}");
            }
        });
        assert!(PROMPT_POST_TIMEOUT < POST_TIMEOUT);
        for event in [
            HookEvent::ToolEnd {
                tool_use_id: "t".into(),
            },
            HookEvent::StatusLine {
                model: None,
                effort: None,
                context: None,
                five_hour: None,
                seven_day: None,
            },
        ] {
            assert_eq!(post_timeout(&event, false), PROMPT_POST_TIMEOUT);
            assert_eq!(post_timeout(&event, true), TLS_PROMPT_POST_TIMEOUT);
        }
        // SessionEnd: stdin wait + POST stay well inside the shared 1.5 s,
        // also over TLS.
        assert!(STDIN_TIMEOUT + POST_TIMEOUT <= Duration::from_millis(800));
        assert!(STDIN_TIMEOUT + TLS_POST_TIMEOUT <= Duration::from_millis(1200));
        assert!(POST_TIMEOUT < TLS_POST_TIMEOUT && PROMPT_POST_TIMEOUT < TLS_PROMPT_POST_TIMEOUT);
    }

    #[test]
    fn permission_requests_carry_the_tool_and_a_capped_preview() {
        let input = serde_json::json!({
            "session_id": "s",
            "hook_event_name": "PermissionRequest",
            "permission_mode": "auto",
            "tool_name": "Bash",
            "tool_input": { "command": "rm -rf \"$DIR\"/", "description": "Clean the build" },
            "permission_suggestions": [],
        });
        let post = build_permission(input.to_string().as_bytes(), "box").unwrap();
        assert_eq!(post.v, VERSION);
        assert_eq!(post.host, "box");
        assert_eq!(post.session_id, "s");
        assert_eq!(post.tool_name, "Bash");
        assert_eq!(post.description, "Clean the build");
        assert_eq!(post.input_preview, r#"{"command":"rm -rf \"$DIR\"/"}"#);

        let long = serde_json::json!({
            "session_id": "s",
            "tool_name": "Write",
            "tool_input": { "content": "й".repeat(MAX_PREVIEW) },
        });
        let post = build_permission(long.to_string().as_bytes(), "box").unwrap();
        assert!(post.input_preview.len() <= MAX_PREVIEW);
        assert!(post.input_preview.ends_with('\u{2026}'));
        assert!(post.description.is_empty());

        for bad in [
            &br#"{"tool_name":"Bash"}"#[..],
            br#"{"session_id":"s"}"#,
            br#"{"session_id":"s","tool_name":"  "}"#,
            br#"{"session_id":"s","tool_name":"Bash","hook_event_name":"Stop"}"#,
            b"not json",
        ] {
            assert!(build_permission(bad, "box").is_err());
        }
        // The other hooks still skip it.
        with_probe(OWN, true, |probe, _| {
            assert!(build("PermissionRequest", input.to_string().as_bytes(), probe).is_err());
        });
    }

    #[test]
    fn decisions_are_claude_code_permission_request_output() {
        let allow: Value = serde_json::from_str(&decision_json(Behavior::Allow)).unwrap();
        assert_eq!(
            allow,
            serde_json::json!({ "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" },
            }})
        );
        let deny: Value = serde_json::from_str(&decision_json(Behavior::Deny)).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            deny["hookSpecificOutput"]["decision"]["message"],
            DENY_MESSAGE
        );
    }

    #[test]
    fn long_texts_are_capped_on_a_char_boundary() {
        let long = "й".repeat(MAX_TEXT); // 2 bytes each
        let input = serde_json::json!({
            "session_id": "s",
            "agent_id": "a",
            "tool_name": "SubagentHandback",
            "tool_input": { "message": long },
        });
        with_probe(OWN, true, |probe, _| {
            let post = build("PostToolUse", input.to_string().as_bytes(), probe).unwrap();
            let HookEvent::SubagentHandback { message, .. } = &post.event else {
                panic!("wrong event");
            };
            assert!(message.len() <= MAX_TEXT);
            assert!(message.ends_with('\u{2026}'));
            let body = serde_json::to_vec(&post).unwrap();
            assert!(body.len() < crate::wire::MAX_HOOK_BODY);
        });
        assert_eq!(cap_text("short".into()), "short");
    }
}

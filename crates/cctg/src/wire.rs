//! Transport contracts between the hub and its per-session companions.
//!
//! Agent link: a persistent TCP connection carrying one JSON object per line.
//! Every line has `"v"` (the protocol version) and `"type"`. The agent sends
//! `hello` with the shared secret, then `register`; the hub answers
//! `registered` or `rejected` and closes. Only the agent reconnects.
//!
//! Hook ingress: one HTTP POST of a [`HookPost`] per hook invocation to
//! [`HOOK_PATH`], secret in `Authorization: Bearer`. The hook mints the
//! [`EventId`] once and sends the same one again only when it re-sends the
//! same POST; the hub drops repeats by it.
//!
//! A new optional field (`#[serde(default)]`) keeps [`VERSION`]; so does a new
//! message type that a peer sends only after the other side announced it in
//! such a field (`permission_ack`, see [`Register::verdict_ack`];
//! `transcript_read`, see [`Register::transcript_reads`]; `console_key`,
//! see [`Register::console_keys`]; `console_command`, see
//! [`Register::console_commands`]; `update` and `released`, see
//! [`Client::self_update`]; `file_start` and the hub's `file_chunk`, see
//! [`Register::files`]; `file_offer` and the agent's `file_chunk`, see
//! [`HubMsg::Registered`]; `session_read` and `session_answer`, see
//! [`Register::session_reads`]; `ping` either way, see
//! [`Register::heartbeat`]). Any other
//! new message type or a changed meaning bumps it. Errors never carry the
//! offending input: a line can contain the secret.

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

pub const VERSION: u32 = 1;
/// Longest accepted agent-link line, newline included.
pub const MAX_LINE: usize = 1 << 20;
/// Longest accepted hook POST body.
pub const MAX_HOOK_BODY: usize = 1 << 20;
pub const HOOK_PATH: &str = "/v1/hook";
/// `cctg doctor` (TASK-031): an empty `POST` that only checks the secret;
/// `204` with it, `401` without, `404` from a hub older than this path.
pub const PING_PATH: &str = "/v1/ping";
pub const MIN_SECRET_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("line longer than {MAX_LINE} bytes")]
    TooLong,
    #[error("message is not valid for its type")]
    Malformed,
    #[error("unsupported or missing protocol version")]
    Version,
    #[error("unknown message type")]
    UnknownKind,
    #[error("connection closed")]
    Closed,
    #[error("connection error: {0:?}")]
    Io(std::io::ErrorKind),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("the shared secret must be at least {MIN_SECRET_LEN} characters")]
    TooShort,
    #[error("the shared secret may contain only visible ASCII characters (no spaces)")]
    Charset,
}

/// The hub shared secret. `Debug` never prints it; comparison is constant-time.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Trims surrounding whitespace. The value goes into an HTTP header, so it
    /// is limited to visible ASCII.
    pub fn parse(value: &str) -> Result<Self, SecretError> {
        let value = value.trim();
        if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(SecretError::Charset);
        }
        if value.len() < MIN_SECRET_LEN {
            return Err(SecretError::TooShort);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, candidate: &[u8]) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate)
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other.0.as_bytes())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Leaks only whether the lengths differ, never where the bytes differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

/// Message types a decoder accepts; keeps unknown types apart from bad fields.
pub trait Kinds {
    const KINDS: &'static [&'static str];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Register {
    /// `CLAUDE_CODE_SESSION_ID` of the Claude Code process that spawned the agent.
    pub session_id: String,
    pub host: String,
    pub cwd: String,
    /// The Claude Code process that spawned the agent. `CLAUDE_CODE_SESSION_ID`
    /// goes stale after `/clear` (the server is not restarted, TASK-013); the
    /// hub follows the process through its `pids` map instead.
    #[serde(default)]
    pub claude_pid: Option<u32>,
    /// The agent answers a `permission_verdict` that carries a `verdict_id`
    /// with `permission_ack`. Agents built before TASK-014 leave it out: the
    /// hub then takes a verdict handed to their link as delivered.
    #[serde(default)]
    pub verdict_ack: bool,
    /// The agent answers `transcript_read` with `transcript_chunk` (TASK-016).
    /// Agents built before leave it out and are never asked.
    #[serde(default)]
    pub transcript_reads: bool,
    /// The agent can press a key in the console of its Claude Code process
    /// and answers `console_key` with `console_key_written` (TASK-029). Only
    /// Windows agents that know their claude pid announce it.
    #[serde(default)]
    pub console_keys: bool,
    /// The agent can type one line into the input box of its Claude Code
    /// console and answers `console_command` with `console_command_typed`
    /// (TASK-043). Only Windows agents that know their claude pid announce it.
    #[serde(default)]
    pub console_commands: bool,
    /// Which cctg build the agent runs and what it can do about a newer one
    /// (TASK-040). Agents built before leave it out: the hub shows them as
    /// outdated and never sends them `update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    /// The agent takes files from the topic: `file_start` and `file_chunk`
    /// (TASK-032). Agents built before leave it out and get a notice in the
    /// topic instead of a file.
    #[serde(default)]
    pub files: bool,
    /// The agent reads its session's files for the hub: it answers
    /// `session_read` with `session_answer`s (TASK-034). Agents built before
    /// leave it out; the hub then shows no `/brief`, no ai-title and builds
    /// subagent blocks from the hooks alone.
    #[serde(default)]
    pub session_reads: bool,
    /// The agent sends `ping` when it wrote nothing for a while and drops
    /// a link that brought nothing for longer ([`Heartbeat`], TASK-049);
    /// the hub does the same once both announced it. Agents built before
    /// leave it out and get no pings.
    #[serde(default)]
    pub heartbeat: bool,
}

/// The agent's build and update abilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    /// `CARGO_PKG_VERSION` of the agent.
    pub version: String,
    /// The agent's build ([`crate::client`]): the commit it was built from,
    /// or with local changes or without git the sha256 of its executable
    /// (TASK-035; before, always that sha256). The hub compares it with its
    /// own, never parses it.
    pub build: String,
    /// The agent runs under the `cctg agent` shim: it answers `update` with
    /// `update_answer` and can hand over to a newer binary without Claude
    /// Code noticing.
    #[serde(default)]
    pub self_update: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub description: String,
    pub input_preview: String,
}

/// Agent to hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello {
        secret: Secret,
    },
    Register(Register),
    /// Text from the channel `reply` tool, for the session's topic.
    Reply {
        text: String,
    },
    PermissionRequest(PermissionRequest),
    /// The agent queued the verdict `verdict_id` for Claude Code. Sent only
    /// to a hub that put the id into the verdict.
    PermissionAck {
        verdict_id: u64,
    },
    /// The answer to one `transcript_read`: the stream events of the complete
    /// lines in `from..to`. `missing`: the file is not there (yet). `more`:
    /// the file has complete lines past `to` that did not fit. `reset`: the
    /// file no longer continues at `from` (it is shorter, or `from` is not
    /// the end of a line): a new file, to be read from its start.
    TranscriptChunk {
        session_id: String,
        from: u64,
        to: u64,
        #[serde(default)]
        lines: Vec<StreamLine>,
        #[serde(default)]
        missing: bool,
        #[serde(default)]
        more: bool,
        #[serde(default)]
        reset: bool,
    },
    /// The answer to one `console_key`: whether the key events were written
    /// into the console input of the agent's Claude Code process. Written
    /// is not handled: Claude Code may not have read the key yet, and Esc
    /// does not always end a turn (it answers an open dialog).
    ConsoleKeyWritten {
        key_id: u64,
        written: bool,
    },
    /// The answer to one `console_command`: what became of the line.
    /// `panel`: the text of the panel the command opened (`/cost`,
    /// `/usage`), which the agent closed again with Esc.
    ConsoleCommandTyped {
        command_id: u64,
        outcome: CommandOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panel: Option<String>,
    },
    /// The answer to one `update`. For `reloading` and `restarting` the
    /// agent leaves: the hub stops handing it messages and answers
    /// `released` behind everything already queued for it.
    UpdateAnswer {
        update_id: u64,
        outcome: UpdateOutcome,
    },
    /// The `send_file` tool (TASK-032) wants to send `size` bytes named
    /// `name` to the session's topic. Sent only to a hub whose `registered`
    /// said `files`. The hub answers `file_answer`: `accepted` asks for the
    /// bytes as `file_chunk`s, anything else ends the transfer.
    FileOffer {
        transfer_id: u64,
        name: String,
        size: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    /// Bytes of an accepted offer ([`FileChunk`]).
    FileChunk(FileChunk),
    /// One answer to the `session_read` with the same `read_id`. A `text`
    /// comes in pieces, in order, until one without `more`; every other
    /// answer comes alone.
    SessionAnswer {
        read_id: u64,
        answer: SessionAnswer,
    },
    /// Only that the link lives ([`Heartbeat`]); sent only to a hub whose
    /// `registered` said `heartbeat`. Never answered.
    Ping,
}

/// What the hub asks the agent to read of its session (TASK-034).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionAsk {
    /// `/brief` or `/full`: the last `prompts` prompts of the transcript,
    /// rendered; answered with `text` pieces.
    Render { view: TranscriptView, prompts: u32 },
    /// The first ai-title after byte `from`; answered with `title`.
    Title { from: u64 },
    /// The `Agent` calls and the agent ids of their results after byte
    /// `from`; answered with `calls`.
    Calls { from: u64 },
    /// The finished block text of subagent `agent_id` of this session;
    /// `path` is its `agent-<id>.jsonl`. Answered with `text` pieces.
    Subagent {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// The running block's header, kept when the meta has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        /// `SubagentStop.last_assistant_message` (the hook caps it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last: Option<String>,
    },
    /// An ask of a newer hub.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptView {
    Brief,
    Full,
}

/// The agent's answer to a `session_read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionAnswer {
    /// A piece of a rendered text; `more`: another piece follows.
    Text {
        text: String,
        #[serde(default)]
        more: bool,
    },
    /// `scanned`: the end of the complete lines looked at (the next ask
    /// starts there).
    Title {
        #[serde(default)]
        title: Option<String>,
        scanned: u64,
    },
    /// `offset`: the end of the complete lines looked at; `more`: lines
    /// past it did not fit.
    Calls {
        offset: u64,
        #[serde(default)]
        calls: Vec<SpawnCall>,
        #[serde(default)]
        links: Vec<SpawnLink>,
        #[serde(default)]
        more: bool,
    },
    /// No such file of this session in the agent's own project folder (not
    /// written yet, or that folder not found yet).
    Missing,
    /// The agent serves nothing: it has no session id or no Claude Code
    /// config folder to find its own project folder in (TASK-034 decision 12).
    Refused,
    /// The file is there but could not be read.
    Unreadable,
    /// Larger than the agent reads or sends.
    TooLarge,
    /// An ask this agent does not know.
    Unsupported,
    /// An answer of a newer agent.
    #[serde(other)]
    Other,
}

/// An `Agent` call of the parent transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnCall {
    /// The tool use id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The result of an `Agent` call that named the subagent it launched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnLink {
    pub agent_id: String,
    pub tool_use_id: String,
}

/// One piece of a file transfer on the agent link, either way: the bytes
/// from `offset` on, standard base64. The pieces of a transfer come in
/// order, each right after the last; the transfer is complete when they
/// reach its size, and broken by anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    pub transfer_id: u64,
    pub offset: u64,
    pub data: String,
}

/// What a Telegram message carried, as the agent names the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Photo,
    Document,
    Video,
    Voice,
    Audio,
    Animation,
    /// A kind of a newer hub.
    #[serde(other)]
    Other,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Document => "document",
            Self::Video => "video",
            Self::Voice => "voice",
            Self::Audio => "audio",
            Self::Animation => "animation",
            Self::Other => "file",
        }
    }
}

/// The hub's answer to a `file_offer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcome {
    /// Send the bytes.
    Accepted,
    /// Telegram took the file.
    Sent,
    /// The session is not the live one of a slot with a topic.
    NoTopic,
    /// Too much waits for Telegram; nothing was taken.
    Busy,
    /// Telegram refused the file, or the transfer broke.
    Failed,
    /// An outcome of a newer hub.
    #[serde(other)]
    Other,
}

/// What an agent does about an `update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// A newer binary is on disk: the agent hands over to it; Claude Code
    /// keeps running.
    Reloading,
    /// Claude must restart to load changed settings or a newer shim: the
    /// agent asked `cctg run` for it and types `/exit`.
    Restarting,
    /// A restart is needed but claude does not run under `cctg run`.
    NeedsManualRestart,
    /// The terminal input holds unsent text; `/exit` was not sent.
    DraftInInput,
    /// A restart is needed, but Claude Code's agent view is open or a
    /// background agent runs ([`crate::keys::agents_block`]): `/exit` was
    /// not sent and the agent stays; the hub asks again later (TASK-047).
    /// Hubs before it read [`Self::Other`], a failure.
    AgentsRunning,
    /// Already up to date: no newer binary, no restart needed.
    UpToDate,
    /// Something failed; nothing changed.
    Failed,
    /// An outcome of a newer agent.
    #[serde(other)]
    Other,
}

/// What an agent did with a `console_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutcome {
    /// The line and Enter went into the console input: the box showed
    /// exactly the line. Not that Claude Code ran it.
    Sent,
    /// The input box held a draft: the typed line was erased, nothing sent.
    Draft,
    /// Claude Code's agent view is open or a background agent runs: nothing
    /// typed (TASK-047). Hubs before it read [`Self::Other`], a failure.
    AgentsRunning,
    /// Not typed, or typed and erased again for another reason.
    Failed,
    /// An outcome of a newer agent.
    #[serde(other)]
    Other,
}

/// A key the agent presses in its Claude Code console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleKey {
    /// Esc: stops the running turn.
    Interrupt,
}

/// The stream events of one transcript line that ends at byte `end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamLine {
    pub end: u64,
    pub items: Vec<StreamItem>,
}

/// [`transcript::StreamEvent`] on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamItem {
    Prompt {
        text: String,
    },
    Channel {
        message_id: i64,
    },
    Note {
        text: String,
    },
    /// The assistant's visible thinking, already cut short by the agent.
    Thinking {
        text: String,
    },
    Call {
        id: String,
        line: String,
    },
    Result {
        id: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// The assistant text that ends a turn; its text comes from `Stop`.
    TurnEnd,
    /// A kind from a newer agent; skipped.
    #[serde(other)]
    Other,
}

impl Kinds for AgentMsg {
    const KINDS: &'static [&'static str] = &[
        "hello",
        "register",
        "reply",
        "permission_request",
        "permission_ack",
        "transcript_chunk",
        "console_key_written",
        "console_command_typed",
        "update_answer",
        "file_offer",
        "file_chunk",
        "session_answer",
        "ping",
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    Auth,
    Version,
    Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Behavior {
    Allow,
    Deny,
}

/// Hub to agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMsg {
    /// `files`: the hub takes `file_offer` (TASK-032). Hubs built before
    /// leave it out.
    Registered {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        files: bool,
        /// The hub keeps the [`Heartbeat`] with an agent that announced
        /// [`Register::heartbeat`] (TASK-049). Hubs built before leave it
        /// out: the agent then sends no pings and waits for the hub as long
        /// as the connection stays open.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        heartbeat: bool,
    },
    Rejected {
        reason: Rejection,
    },
    /// A Telegram message for `notifications/claude/channel`.
    Inbound {
        content: String,
        #[serde(default)]
        meta: BTreeMap<String, String>,
    },
    PermissionVerdict {
        request_id: String,
        behavior: Behavior,
        /// Set only for an agent that registered with `verdict_ack`; the same
        /// id comes again when the hub re-sends the same answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict_id: Option<u64>,
    },
    /// Sent only to an agent that registered with `transcript_reads`: read
    /// the complete lines of the transcript of `session_id` from byte `from`
    /// (`None`: from the end of its last complete line) and answer with one
    /// `transcript_chunk`. `path` is the hook's `transcript_path`, still sent
    /// for agents before TASK-034; a newer agent never opens it and reads
    /// `<own project folder>/<session_id>.jsonl`.
    TranscriptRead {
        session_id: String,
        path: String,
        #[serde(default)]
        from: Option<u64>,
    },
    /// Sent only to an agent that registered with `console_keys`: press
    /// `key` in the console of its Claude Code process and answer with one
    /// `console_key_written` carrying the same `key_id`.
    ConsoleKey {
        key_id: u64,
        key: ConsoleKey,
    },
    /// Sent only to an agent that registered with `console_commands`: type
    /// `text` (one line, a `!` bash command or a slash command) into the
    /// input box of its Claude Code console, as a user at the terminal
    /// would, and answer with one `console_command_typed` carrying the same
    /// `command_id`.
    ConsoleCommand {
        command_id: u64,
        text: String,
    },
    /// Sent only to an agent whose [`Client::self_update`] is set, on the
    /// user's "Обновить": take a newer binary, or restart claude when
    /// needed; answered with one `update_answer`.
    Update {
        update_id: u64,
    },
    /// The answer to a leaving `update_answer`, queued behind every message
    /// handed to the agent before it: after it nothing more comes.
    /// `session_id`: the session the hub had bound the agent to (after
    /// `/clear` not the agent's env id), for `claude --resume`.
    Released {
        update_id: u64,
        session_id: String,
    },
    /// Sent only to an agent that registered with `files`: a file from the
    /// topic follows as `file_chunk`s of `size` bytes in all. Once they are
    /// all there the agent saves it as `name` and hands Claude Code
    /// `content` and `meta` like an `inbound`, with where the file is.
    FileStart {
        transfer_id: u64,
        name: String,
        size: u64,
        kind: FileKind,
        #[serde(default)]
        content: String,
        #[serde(default)]
        meta: BTreeMap<String, String>,
    },
    /// Bytes of the file of a `file_start` ([`FileChunk`]).
    FileChunk(FileChunk),
    /// The answer to a `file_offer`; `accepted` comes first, then `sent`
    /// or `failed` once Telegram answered.
    FileAnswer {
        transfer_id: u64,
        outcome: FileOutcome,
    },
    /// Sent only to an agent that registered with `session_reads`: read the
    /// files of session `session_id` for `ask` and answer with
    /// `session_answer`s carrying the same `read_id`. The agent builds the
    /// paths itself under its own project folder (the session's transcript,
    /// its `subagents/` files for a subagent ask); a `path` an earlier hub
    /// sends is ignored.
    SessionRead {
        read_id: u64,
        session_id: String,
        ask: SessionAsk,
    },
    /// Only that the link lives ([`Heartbeat`]); sent only to an agent that
    /// registered with `heartbeat`. Never answered.
    Ping,
}

impl Kinds for HubMsg {
    const KINDS: &'static [&'static str] = &[
        "registered",
        "rejected",
        "inbound",
        "permission_verdict",
        "transcript_read",
        "console_key",
        "console_command",
        "update",
        "released",
        "file_start",
        "file_chunk",
        "file_answer",
        "session_read",
        "ping",
    ];
}

/// Idle ping and dead-peer timeout of the agent link (TASK-049). A half-open
/// connection (a NAT or tunnel on the way forgot it) never reports an
/// error: only the silence of the peer shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// A side that wrote nothing for this long sends `ping`.
    pub interval: Duration,
    /// A side that read nothing for this long drops the link.
    pub timeout: Duration,
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            interval: HEARTBEAT_INTERVAL,
            timeout: HEARTBEAT_TIMEOUT,
        }
    }
}

/// What a link's [`Liveness`] asks for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    /// Nothing written for the interval: send `ping`.
    Ping,
    /// Nothing read for the timeout: the peer is gone.
    Dead,
}

/// When one end of a link last read and wrote; off without a heartbeat.
#[derive(Debug, Clone, Copy)]
pub struct Liveness {
    heartbeat: Option<Heartbeat>,
    heard: Instant,
    said: Instant,
}

impl Liveness {
    pub fn new(heartbeat: Option<Heartbeat>) -> Self {
        let now = Instant::now();
        Self {
            heartbeat,
            heard: now,
            said: now,
        }
    }

    /// A line came from the peer.
    pub fn heard(&mut self) {
        self.heard = Instant::now();
    }

    /// A line went to the peer.
    pub fn said(&mut self) {
        self.said = Instant::now();
    }

    /// The next beat and when it is due; `None` without a heartbeat.
    pub fn next(&self) -> Option<(Instant, Beat)> {
        let heartbeat = self.heartbeat?;
        let dead = self.heard + heartbeat.timeout;
        let ping = self.said + heartbeat.interval;
        Some(if dead <= ping {
            (dead, Beat::Dead)
        } else {
            (ping, Beat::Ping)
        })
    }
}

/// Waits for `next` ([`Liveness::next`]); never ends for `None`. Takes the
/// value, not the [`Liveness`], so a `select!` branch borrows nothing.
pub async fn beat(next: Option<(Instant, Beat)>) -> Beat {
    match next {
        Some((at, beat)) => {
            tokio::time::sleep_until(at).await;
            beat
        }
        None => std::future::pending().await,
    }
}

/// One agent-link line: the message plus `"v"`, newline-terminated.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    #[derive(Serialize)]
    struct Framed<'a, T> {
        v: u32,
        #[serde(flatten)]
        msg: &'a T,
    }
    let mut line = serde_json::to_vec(&Framed { v: VERSION, msg })
        .expect("wire messages have string keys and always serialize");
    line.push(b'\n');
    line
}

/// Checks the version, then the type, then the fields.
pub fn decode<T: DeserializeOwned + Kinds>(line: &[u8]) -> Result<T, WireError> {
    let value: Value = serde_json::from_slice(line).map_err(|_| WireError::Malformed)?;
    check_version(&value)?;
    check_kind::<T>(value.get("type"))?;
    serde_json::from_value(value).map_err(|_| WireError::Malformed)
}

fn check_version(value: &Value) -> Result<(), WireError> {
    match value.get("v").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => Ok(()),
        _ => Err(WireError::Version),
    }
}

fn check_kind<T: Kinds>(kind: Option<&Value>) -> Result<(), WireError> {
    match kind.and_then(Value::as_str) {
        Some(kind) if T::KINDS.contains(&kind) => Ok(()),
        Some(_) => Err(WireError::UnknownKind),
        None => Err(WireError::Malformed),
    }
}

/// Resumes reading one line into `buf`. Reads at most [`MAX_LINE`] bytes, so
/// a peer that never sends a newline cannot grow `buf` further.
///
/// A cancelled call keeps any bytes already read. After `Ok(())`, the caller
/// must consume the complete line and clear `buf` before starting the next.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> Result<(), WireError> {
    read_line_max(reader, buf, MAX_LINE).await
}

/// [`read_line`] with the lower cap `max` (the hub's line before the secret
/// is checked).
pub async fn read_line_max<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> Result<(), WireError> {
    if buf.last() == Some(&b'\n') {
        return Ok(());
    }
    if buf.len() >= max {
        return Err(WireError::TooLong);
    }
    let remaining = max - buf.len();
    let read = (&mut *reader)
        .take(remaining as u64)
        .read_until(b'\n', buf)
        .await
        .map_err(|error| WireError::Io(error.kind()))?;
    match buf.last() {
        Some(b'\n') => Ok(()),
        _ if read >= remaining => Err(WireError::TooLong),
        _ => Err(WireError::Closed),
    }
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<(), WireError> {
    let io = |error: std::io::Error| WireError::Io(error.kind());
    writer.write_all(&encode(msg)).await.map_err(io)?;
    writer.flush().await.map_err(io)
}

/// Per-invocation hook event key: 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EventId(String);

impl EventId {
    /// A fresh id. Unique, not secret: requests are authenticated separately.
    pub fn new() -> Self {
        Self(format!("{:016x}{:016x}", random_u64(), random_u64()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<String> for EventId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(value))
        } else {
            Err("event_id must be 32 lowercase hex digits")
        }
    }
}

impl From<EventId> for String {
    fn from(id: EventId) -> Self {
        id.0
    }
}

/// 64 bits from a randomly keyed SipHash (the std `RandomState` keys come
/// from the OS generator) over a counter, the clock and the pid. No `rand`
/// crate: ids and jitter need uniqueness, not cryptographic strength.
pub(crate) fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default(),
    );
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// Body of one hook POST. Paths are strings: they may come from another OS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookPost {
    pub v: u32,
    pub event_id: EventId,
    pub host: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub transcript_path: String,
    pub event: HookEvent,
    /// Pids of the claude processes alive on `host` when the hook ran
    /// (`SessionStart`/`SessionEnd` only; see
    /// [`proctree::live_claude_pids`](crate::proctree::live_claude_pids)).
    /// The hub ends that host's sessions whose process is not listed. Never
    /// kept in the spool: a late list would end sessions started after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_claude_pids: Option<Vec<u32>>,
    /// `CARGO_PKG_VERSION` of the hook (TASK-040), for the hub log only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

impl HookPost {
    /// Mints the event id; keep the value to re-send the same POST.
    pub fn new(
        host: String,
        session_id: String,
        cwd: String,
        transcript_path: String,
        event: HookEvent,
    ) -> Self {
        Self {
            v: VERSION,
            event_id: EventId::new(),
            host,
            session_id,
            cwd,
            transcript_path,
            event,
            live_claude_pids: None,
            client_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }
    }
}

/// Event-specific fields; `TASK-012` fills them from the hook stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart {
        #[serde(default)]
        source: Option<String>,
        /// `CLAUDE_PID` of the hook: the session's own claude process.
        #[serde(default)]
        claude_pid: Option<u32>,
        /// Next `claude` ancestor in the process tree, when there is one.
        #[serde(default)]
        parent_claude_pid: Option<u32>,
    },
    SessionEnd {
        #[serde(default)]
        reason: Option<String>,
        /// `CLAUDE_PID` of the hook. Tells the end of a nested
        /// `claude -p --resume <id>` from the end of the session itself.
        #[serde(default)]
        claude_pid: Option<u32>,
    },
    UserPromptSubmit {
        #[serde(default)]
        prompt_id: Option<String>,
    },
    Stop {
        #[serde(default)]
        prompt_id: Option<String>,
        #[serde(default)]
        last_assistant_message: Option<String>,
    },
    SubagentStart {
        agent_id: String,
        #[serde(default)]
        agent_type: String,
    },
    SubagentStop {
        agent_id: String,
        #[serde(default)]
        agent_type: String,
        #[serde(default)]
        agent_transcript_path: Option<String>,
        #[serde(default)]
        last_assistant_message: Option<String>,
    },
    /// `SubagentHandback` report, taken from `PreToolUse`/`PostToolUse`.
    SubagentHandback { agent_id: String, message: String },
    /// A tool call of the main conversation started (`PreToolUse`, TASK-029).
    /// `line`: its `/brief` line.
    ToolStart { tool_use_id: String, line: String },
    /// A tool call of the main conversation ended (`PostToolUse` or
    /// `PostToolUseFailure`).
    ToolEnd { tool_use_id: String },
    /// Numbers from Claude Code's status line input (`cctg statusline`):
    /// percentages rounded to whole numbers, each absent when Claude Code
    /// did not give it.
    StatusLine {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        context: Option<u32>,
        #[serde(default)]
        five_hour: Option<u32>,
        #[serde(default)]
        seven_day: Option<u32>,
    },
}

impl HookEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStart { .. } => "session_start",
            Self::SessionEnd { .. } => "session_end",
            Self::UserPromptSubmit { .. } => "user_prompt_submit",
            Self::Stop { .. } => "stop",
            Self::SubagentStart { .. } => "subagent_start",
            Self::SubagentStop { .. } => "subagent_stop",
            Self::SubagentHandback { .. } => "subagent_handback",
            Self::ToolStart { .. } => "tool_start",
            Self::ToolEnd { .. } => "tool_end",
            Self::StatusLine { .. } => "status_line",
        }
    }

    /// Events that come many times a turn; the hub logs them at debug only.
    pub fn is_frequent(&self) -> bool {
        matches!(
            self,
            Self::ToolStart { .. } | Self::ToolEnd { .. } | Self::StatusLine { .. }
        )
    }
}

impl Kinds for HookEvent {
    const KINDS: &'static [&'static str] = &[
        "session_start",
        "session_end",
        "user_prompt_submit",
        "stop",
        "subagent_start",
        "subagent_stop",
        "subagent_handback",
        "tool_start",
        "tool_end",
        "status_line",
    ];
}

pub fn decode_hook(body: &[u8]) -> Result<HookPost, WireError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| WireError::Malformed)?;
    check_version(&value)?;
    check_kind::<HookEvent>(value.get("event").and_then(|event| event.get("type")))?;
    serde_json::from_value(value).map_err(|_| WireError::Malformed)
}

/// Body of one `PermissionRequest` hook POST to [`PERMISSION_PATH`]. The hub
/// holds the request open until it has an answer: `200` with a
/// [`PermissionAnswer`] when the user decided in Telegram, `204` when it has
/// no decision (the channel relays the same request, the wait ran out, the
/// session ended, the hub stopped). A hub without the path answers `404`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionPost {
    pub v: u32,
    pub host: String,
    pub session_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_preview: String,
}

pub const PERMISSION_PATH: &str = "/v1/permission";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAnswer {
    pub behavior: Behavior,
}

pub fn decode_permission(body: &[u8]) -> Result<PermissionPost, WireError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| WireError::Malformed)?;
    check_version(&value)?;
    serde_json::from_value(value).map_err(|_| WireError::Malformed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    const SECRET: &str = "0123456789abcdef-secret";

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    fn agent_samples() -> Vec<AgentMsg> {
        vec![
            AgentMsg::Hello { secret: secret() },
            AgentMsg::Register(Register {
                session_id: "5e551017-0000-4000-8000-000000000001".into(),
                host: "box".into(),
                cwd: "C:\\work\\app".into(),
                claude_pid: Some(4242),
                verdict_ack: true,
                transcript_reads: true,
                console_keys: true,
                console_commands: true,
                client: None,
                files: true,
                session_reads: true,
                heartbeat: true,
            }),
            AgentMsg::Reply {
                text: "multi\nline \u{2014} text".into(),
            },
            AgentMsg::PermissionRequest(PermissionRequest {
                request_id: "abcde".into(),
                tool_name: "Bash".into(),
                description: "run tests".into(),
                input_preview: "{\"command\":\"cargo test\"}".into(),
            }),
            AgentMsg::PermissionAck {
                verdict_id: u64::MAX,
            },
            AgentMsg::TranscriptChunk {
                session_id: "s".into(),
                from: 10,
                to: 20,
                lines: vec![StreamLine {
                    end: 20,
                    items: vec![
                        StreamItem::Prompt { text: "p".into() },
                        StreamItem::Channel { message_id: 7 },
                        StreamItem::Note { text: "n".into() },
                        StreamItem::Call {
                            id: "t1".into(),
                            line: "• Bash: x".into(),
                        },
                        StreamItem::Result {
                            id: "t1".into(),
                            error: Some("boom".into()),
                        },
                        StreamItem::TurnEnd,
                    ],
                }],
                missing: false,
                more: true,
                reset: false,
            },
            AgentMsg::ConsoleKeyWritten {
                key_id: u64::MAX,
                written: true,
            },
            AgentMsg::ConsoleCommandTyped {
                command_id: u64::MAX,
                outcome: CommandOutcome::Draft,
                panel: None,
            },
            AgentMsg::ConsoleCommandTyped {
                command_id: 1,
                outcome: CommandOutcome::Sent,
                panel: Some("Total cost: $0.01".into()),
            },
            AgentMsg::UpdateAnswer {
                update_id: 9,
                outcome: UpdateOutcome::DraftInInput,
            },
            AgentMsg::UpdateAnswer {
                update_id: 10,
                outcome: UpdateOutcome::AgentsRunning,
            },
            AgentMsg::ConsoleCommandTyped {
                command_id: 2,
                outcome: CommandOutcome::AgentsRunning,
                panel: None,
            },
            AgentMsg::FileOffer {
                transfer_id: u64::MAX,
                name: "\u{448}\u{43e}\u{442} \"1\".png".into(),
                size: 3,
                caption: Some("see".into()),
            },
            AgentMsg::FileChunk(FileChunk {
                transfer_id: 1,
                offset: 0,
                data: "AAEC".into(),
            }),
            AgentMsg::SessionAnswer {
                read_id: 5,
                answer: SessionAnswer::Text {
                    text: "> p\n\u{2014}".into(),
                    more: true,
                },
            },
            AgentMsg::SessionAnswer {
                read_id: 6,
                answer: SessionAnswer::Calls {
                    offset: 9,
                    calls: vec![SpawnCall {
                        id: "t1".into(),
                        subagent_type: Some("Explore".into()),
                        description: None,
                    }],
                    links: vec![SpawnLink {
                        agent_id: "a1".into(),
                        tool_use_id: "t1".into(),
                    }],
                    more: false,
                },
            },
            AgentMsg::SessionAnswer {
                read_id: 7,
                answer: SessionAnswer::Title {
                    title: Some("t".into()),
                    scanned: 3,
                },
            },
            AgentMsg::SessionAnswer {
                read_id: 8,
                answer: SessionAnswer::Missing,
            },
            AgentMsg::SessionAnswer {
                read_id: 9,
                answer: SessionAnswer::Refused,
            },
            AgentMsg::Ping,
        ]
    }

    fn hub_samples() -> Vec<HubMsg> {
        vec![
            HubMsg::Registered {
                files: false,
                heartbeat: false,
            },
            HubMsg::Registered {
                files: true,
                heartbeat: true,
            },
            HubMsg::Ping,
            HubMsg::Rejected {
                reason: Rejection::Auth,
            },
            HubMsg::Rejected {
                reason: Rejection::Version,
            },
            HubMsg::Rejected {
                reason: Rejection::Protocol,
            },
            HubMsg::Inbound {
                content: "hi".into(),
                meta: [("target_agent".to_owned(), "a1".to_owned())].into(),
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Deny,
                verdict_id: Some(u64::MAX),
            },
            HubMsg::TranscriptRead {
                session_id: "s".into(),
                path: "/p/s.jsonl".into(),
                from: None,
            },
            HubMsg::ConsoleKey {
                key_id: 7,
                key: ConsoleKey::Interrupt,
            },
            HubMsg::ConsoleCommand {
                command_id: 8,
                text: "!echo \u{2014} hi".into(),
            },
            HubMsg::Update { update_id: 9 },
            HubMsg::Released {
                update_id: 9,
                session_id: "s".into(),
            },
            HubMsg::FileStart {
                transfer_id: 4,
                name: "photo.jpg".into(),
                size: 3,
                kind: FileKind::Photo,
                content: "caption".into(),
                meta: [("message_id".to_owned(), "5".to_owned())].into(),
            },
            HubMsg::FileChunk(FileChunk {
                transfer_id: 4,
                offset: 0,
                data: "AAEC".into(),
            }),
            HubMsg::FileAnswer {
                transfer_id: 4,
                outcome: FileOutcome::Accepted,
            },
            HubMsg::SessionRead {
                read_id: 5,
                session_id: "s".into(),
                ask: SessionAsk::Render {
                    view: TranscriptView::Full,
                    prompts: 2,
                },
            },
            HubMsg::SessionRead {
                read_id: 6,
                session_id: "s".into(),
                ask: SessionAsk::Subagent {
                    agent_id: "a1".into(),
                    agent_type: Some("Explore".into()),
                    description: None,
                    header: None,
                    last: Some("done".into()),
                },
            },
        ]
    }

    fn hook_samples() -> Vec<HookEvent> {
        vec![
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
            HookEvent::SessionEnd {
                reason: Some("prompt_input_exit".into()),
                claude_pid: Some(10),
            },
            HookEvent::UserPromptSubmit {
                prompt_id: Some("p1".into()),
            },
            HookEvent::Stop {
                prompt_id: Some("p1".into()),
                last_assistant_message: Some("done".into()),
            },
            HookEvent::SubagentStart {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
            },
            HookEvent::SubagentStop {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
                agent_transcript_path: Some("/x/agent-a1.jsonl".into()),
                last_assistant_message: None,
            },
            HookEvent::SubagentHandback {
                agent_id: "a1".into(),
                message: "report".into(),
            },
            HookEvent::ToolStart {
                tool_use_id: "toolu_1".into(),
                line: "• Bash: run tests".into(),
            },
            HookEvent::ToolEnd {
                tool_use_id: "toolu_1".into(),
            },
            HookEvent::StatusLine {
                model: Some("Opus".into()),
                effort: Some("high".into()),
                context: Some(50),
                five_hour: Some(3),
                seven_day: None,
            },
        ]
    }

    fn kind_of(line: &[u8]) -> String {
        let value: Value = serde_json::from_slice(line).unwrap();
        value["type"].as_str().unwrap().to_owned()
    }

    #[test]
    fn every_link_message_round_trips_as_one_versioned_line() {
        for msg in agent_samples() {
            let line = encode(&msg);
            assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
            assert!(line.ends_with(b"\n"));
            assert_eq!(serde_json::from_slice::<Value>(&line).unwrap()["v"], 1);
            assert_eq!(decode::<AgentMsg>(&line), Ok(msg));
        }
        for msg in hub_samples() {
            assert_eq!(decode::<HubMsg>(&encode(&msg)), Ok(msg));
        }
    }

    /// TASK-047: the new outcomes are plain names, which a hub before them
    /// reads as `other` (see the `teleported` and `queued` cases).
    #[test]
    fn background_agent_outcomes_have_their_own_names() {
        let update = encode(&AgentMsg::UpdateAnswer {
            update_id: 1,
            outcome: UpdateOutcome::AgentsRunning,
        });
        let command = encode(&AgentMsg::ConsoleCommandTyped {
            command_id: 1,
            outcome: CommandOutcome::AgentsRunning,
            panel: None,
        });
        for line in [update, command] {
            let value: Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(value["outcome"], "agents_running");
        }
    }

    #[test]
    fn kind_lists_match_the_enums() {
        let agent: HashSet<String> = agent_samples()
            .iter()
            .map(|m| kind_of(&encode(m)))
            .collect();
        assert_eq!(
            agent,
            AgentMsg::KINDS.iter().map(|k| k.to_string()).collect()
        );
        let hub: HashSet<String> = hub_samples().iter().map(|m| kind_of(&encode(m))).collect();
        assert_eq!(hub, HubMsg::KINDS.iter().map(|k| k.to_string()).collect());
        let hooks: Vec<&str> = hook_samples().iter().map(HookEvent::kind).collect();
        assert_eq!(hooks, HookEvent::KINDS);
        for event in hook_samples() {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["type"], event.kind());
        }
    }

    #[test]
    fn bad_lines_are_typed_errors() {
        let cases: [(&[u8], WireError); 8] = [
            (br#"{"v":2,"type":"reply","text":"x"}"#, WireError::Version),
            (br#"{"type":"reply","text":"x"}"#, WireError::Version),
            (
                br#"{"v":"1","type":"reply","text":"x"}"#,
                WireError::Version,
            ),
            (
                br#"{"v":1,"type":"teleport","text":"x"}"#,
                WireError::UnknownKind,
            ),
            (br#"{"v":1,"text":"x"}"#, WireError::Malformed),
            (br#"{"v":1,"type":"reply"}"#, WireError::Malformed),
            (b"not json at all", WireError::Malformed),
            (b"", WireError::Malformed),
        ];
        for (line, want) in cases {
            assert_eq!(
                decode::<AgentMsg>(line),
                Err(want),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        assert_eq!(
            decode::<HubMsg>(br#"{"v":1,"type":"hello","secret":"x"}"#),
            Err(WireError::UnknownKind)
        );
    }

    #[test]
    fn a_register_without_claude_pid_still_decodes() {
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                session_id: "s".into(),
                host: "h".into(),
                cwd: "/w".into(),
                claude_pid: None,
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                heartbeat: false,
            }))
        );
    }

    #[test]
    fn console_keys_and_status_events_stay_compatible_with_version_one_peers() {
        // An agent before TASK-029 never announces console keys; a hub
        // before it never sends one, so the new kinds stay behind the flag.
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","console_keys":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                console_keys: true,
                console_commands: false,
                client: None,
                ..
            }))
        ));
        let key = encode(&HubMsg::ConsoleKey {
            key_id: 3,
            key: ConsoleKey::Interrupt,
        });
        let value: Value = serde_json::from_slice(&key).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["key"], "interrupt");
        // Esc is the only key.
        for other in ["reboot", "esc"] {
            let line = format!(r#"{{"v":1,"type":"console_key","key_id":3,"key":"{other}"}}"#);
            assert_eq!(decode::<HubMsg>(line.as_bytes()), Err(WireError::Malformed));
        }
        // A command outcome of a newer agent is still an answer.
        let typed = br#"{"v":1,"type":"console_command_typed","command_id":4,"outcome":"queued"}"#;
        assert_eq!(
            decode::<AgentMsg>(typed),
            Ok(AgentMsg::ConsoleCommandTyped {
                command_id: 4,
                outcome: CommandOutcome::Other,
                panel: None,
            })
        );
        // A status line without numbers is still one event.
        let id = EventId::new();
        let body = json!({ "v": 1, "event_id": id.as_str(), "host": "h", "session_id": "s",
            "event": { "type": "status_line" } });
        assert_eq!(
            decode_hook(&serde_json::to_vec(&body).unwrap()).map(|post| post.event),
            Ok(HookEvent::StatusLine {
                model: None,
                effort: None,
                context: None,
                five_hour: None,
                seven_day: None,
            })
        );
        assert!(
            HookEvent::ToolEnd {
                tool_use_id: "t".into()
            }
            .is_frequent()
        );
        assert!(!HookEvent::UserPromptSubmit { prompt_id: None }.is_frequent());
    }

    #[test]
    fn verdict_acks_stay_compatible_with_version_one_peers() {
        // A hub before TASK-014 sends no id; an agent before it reads past one.
        let legacy =
            br#"{"v":1,"type":"permission_verdict","request_id":"abcde","behavior":"allow"}"#;
        assert_eq!(
            decode::<HubMsg>(legacy),
            Ok(HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            })
        );
        let without_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Allow,
            verdict_id: None,
        });
        assert_eq!(without_id, [&legacy[..], b"\n"].concat());
        let with_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Deny,
            verdict_id: Some(7),
        });
        let value: Value = serde_json::from_slice(&with_id).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["verdict_id"], 7);
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","verdict_ack":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                verdict_ack: true,
                ..
            }))
        ));
        assert_eq!(
            decode::<AgentMsg>(br#"{"v":1,"type":"permission_ack"}"#),
            Err(WireError::Malformed)
        );
    }

    #[test]
    fn transcript_chunks_stay_readable_across_agent_versions() {
        // A kind from a newer agent is skipped, not a broken chunk.
        let line = br#"{"v":1,"type":"transcript_chunk","session_id":"s","from":0,"to":9,"lines":[{"end":9,"items":[{"kind":"diff","x":1},{"kind":"channel","message_id":3}]}]}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::TranscriptChunk {
                session_id: "s".into(),
                from: 0,
                to: 9,
                lines: vec![StreamLine {
                    end: 9,
                    items: vec![StreamItem::Other, StreamItem::Channel { message_id: 3 }],
                }],
                missing: false,
                more: false,
                reset: false,
            })
        );
        let read = br#"{"v":1,"type":"transcript_read","session_id":"s","path":"/p"}"#;
        assert!(matches!(
            decode::<HubMsg>(read),
            Ok(HubMsg::TranscriptRead { from: None, .. })
        ));
    }

    /// TASK-049: the heartbeat is announced both ways; a peer before it sees
    /// the lines it saw before and never gets a `ping`.
    #[test]
    fn heartbeats_stay_compatible_with_version_one_peers() {
        assert_eq!(
            encode(&HubMsg::Ping),
            b"{\"v\":1,\"type\":\"ping\"}
"
        );
        assert_eq!(
            encode(&AgentMsg::Ping),
            b"{\"v\":1,\"type\":\"ping\"}
"
        );
        // A hub before it: no heartbeat.
        assert_eq!(
            decode::<HubMsg>(br#"{"v":1,"type":"registered","files":true}"#),
            Ok(HubMsg::Registered {
                files: true,
                heartbeat: false,
            })
        );
        // An agent before it: no heartbeat.
        let old = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        match decode::<AgentMsg>(old) {
            Ok(AgentMsg::Register(register)) => assert!(!register.heartbeat),
            other => panic!("{other:?}"),
        }
        let line =
            br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","heartbeat":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                heartbeat: true,
                ..
            }))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn liveness_pings_when_quiet_and_dies_when_deaf() {
        assert_eq!(Liveness::new(None).next(), None);
        let heartbeat = Heartbeat {
            interval: Duration::from_secs(3),
            timeout: Duration::from_secs(9),
        };
        let start = Instant::now();
        let mut live = Liveness::new(Some(heartbeat));
        assert_eq!(live.next(), Some((start + heartbeat.interval, Beat::Ping)));
        tokio::time::advance(Duration::from_secs(8)).await;
        live.said();
        // One second to the timeout, three to the next ping.
        assert_eq!(live.next(), Some((start + heartbeat.timeout, Beat::Dead)));
        live.heard();
        assert_eq!(
            live.next(),
            Some((start + Duration::from_secs(11), Beat::Ping))
        );
        assert_eq!(beat(live.next()).await, Beat::Ping);
        assert_eq!(Instant::now(), start + Duration::from_secs(11));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let line = br#"{"v":1,"type":"registered","extra":{"a":1}}"#;
        assert_eq!(
            decode::<HubMsg>(line),
            Ok(HubMsg::Registered {
                files: false,
                heartbeat: false,
            })
        );
        let line = br#"{"v":1,"type":"reply","text":"t","later":true}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Reply { text: "t".into() })
        );
    }

    #[test]
    fn client_builds_and_updates_stay_compatible_with_version_one_peers() {
        // An agent before TASK-040 sends no client; the hub reads it as None.
        let legacy = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        match decode::<AgentMsg>(legacy) {
            Ok(AgentMsg::Register(register)) => assert_eq!(register.client, None),
            other => panic!("{other:?}"),
        }
        let register = Register {
            session_id: "s".into(),
            host: "h".into(),
            cwd: "/w".into(),
            claude_pid: None,
            verdict_ack: true,
            transcript_reads: true,
            console_keys: true,
            console_commands: false,
            files: false,
            session_reads: false,
            heartbeat: false,
            client: Some(Client {
                version: "0.1.0".into(),
                build: "ab".repeat(32),
                self_update: true,
            }),
        };
        let line = encode(&AgentMsg::Register(register.clone()));
        assert_eq!(decode::<AgentMsg>(&line), Ok(AgentMsg::Register(register)));
        // Without a client the field stays out of the line: older hubs see
        // exactly what they saw before.
        let bare = encode(&AgentMsg::Register(Register {
            client: None,
            ..match decode::<AgentMsg>(legacy) {
                Ok(AgentMsg::Register(register)) => register,
                other => panic!("{other:?}"),
            }
        }));
        assert!(!String::from_utf8_lossy(&bare).contains("client"));
        // An outcome a newer agent invents is read, not refused.
        let newer = br#"{"v":1,"type":"update_answer","update_id":3,"outcome":"teleported"}"#;
        assert_eq!(
            decode::<AgentMsg>(newer),
            Ok(AgentMsg::UpdateAnswer {
                update_id: 3,
                outcome: UpdateOutcome::Other
            })
        );
        // Hook posts carry the hook's version; older ones leave it out.
        let post = HookPost::new(
            "h".into(),
            "s".into(),
            String::new(),
            String::new(),
            HookEvent::UserPromptSubmit { prompt_id: None },
        );
        assert_eq!(
            post.client_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let mut old = serde_json::to_value(&post).unwrap();
        old.as_object_mut().unwrap().remove("client_version");
        let old = decode_hook(&serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(old.client_version, None);
    }

    #[test]
    fn files_stay_compatible_with_version_one_peers() {
        // A hub before TASK-032 answers the bare registration line, which a
        // newer agent reads as a hub that takes no files; an older agent
        // reads a newer hub's line as before.
        let legacy = br#"{"v":1,"type":"registered"}"#;
        assert_eq!(
            decode::<HubMsg>(legacy),
            Ok(HubMsg::Registered {
                files: false,
                heartbeat: false,
            })
        );
        assert_eq!(
            encode(&HubMsg::Registered {
                files: false,
                heartbeat: false,
            }),
            [&legacy[..], b"\n"].concat()
        );
        let newer: Value = serde_json::from_slice(&encode(&HubMsg::Registered {
            files: true,
            heartbeat: false,
        }))
        .unwrap();
        assert_eq!((&newer["v"], &newer["files"]), (&json!(1), &json!(true)));
        // An agent before TASK-032 never announces files.
        let old = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        match decode::<AgentMsg>(old) {
            Ok(AgentMsg::Register(register)) => assert!(!register.files),
            other => panic!("{other:?}"),
        }
        // Kinds and outcomes of a newer peer are read, not refused.
        let start =
            br#"{"v":1,"type":"file_start","transfer_id":1,"name":"n","size":0,"kind":"sticker"}"#;
        assert!(matches!(
            decode::<HubMsg>(start),
            Ok(HubMsg::FileStart { kind: FileKind::Other, ref content, .. }) if content.is_empty()
        ));
        let answer = br#"{"v":1,"type":"file_answer","transfer_id":1,"outcome":"queued"}"#;
        assert_eq!(
            decode::<HubMsg>(answer),
            Ok(HubMsg::FileAnswer {
                transfer_id: 1,
                outcome: FileOutcome::Other
            })
        );
        // Both directions carry the same flat chunk line.
        let chunk = FileChunk {
            transfer_id: 2,
            offset: 7,
            data: "QQ==".into(),
        };
        let line = encode(&AgentMsg::FileChunk(chunk.clone()));
        assert_eq!(line, encode(&HubMsg::FileChunk(chunk)));
        assert!(
            String::from_utf8_lossy(&line)
                .starts_with(r#"{"v":1,"type":"file_chunk","transfer_id":2,"offset":7"#)
        );
        assert_eq!(FileKind::Voice.as_str(), "voice");
    }

    #[test]
    fn session_reads_stay_compatible_with_version_one_peers() {
        // An agent before TASK-034 never announces session reads.
        let old = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        match decode::<AgentMsg>(old) {
            Ok(AgentMsg::Register(register)) => assert!(!register.session_reads),
            other => panic!("{other:?}"),
        }
        // An ask or an answer of a newer peer is read, not refused; the
        // `path` an earlier hub sends is dropped unread.
        let ask = br#"{"v":1,"type":"session_read","read_id":1,"session_id":"s","path":"p","ask":{"kind":"diff","x":1}}"#;
        assert!(matches!(
            decode::<HubMsg>(ask),
            Ok(HubMsg::SessionRead {
                ask: SessionAsk::Other,
                ..
            })
        ));
        let answer = br#"{"v":1,"type":"session_answer","read_id":1,"answer":{"kind":"queued"}}"#;
        assert_eq!(
            decode::<AgentMsg>(answer),
            Ok(AgentMsg::SessionAnswer {
                read_id: 1,
                answer: SessionAnswer::Other
            })
        );
        // The last piece of a text leaves `more` out.
        let last =
            br#"{"v":1,"type":"session_answer","read_id":2,"answer":{"kind":"text","text":"x"}}"#;
        assert_eq!(
            decode::<AgentMsg>(last),
            Ok(AgentMsg::SessionAnswer {
                read_id: 2,
                answer: SessionAnswer::Text {
                    text: "x".into(),
                    more: false
                }
            })
        );
    }

    #[test]
    fn errors_and_debug_never_show_the_secret() {
        let line = format!(r#"{{"v":1,"type":"register","session_id":7,"host":"{SECRET}"}}"#);
        let error = decode::<AgentMsg>(line.as_bytes()).unwrap_err();
        assert_eq!(error, WireError::Malformed);
        assert!(!format!("{error} {error:?}").contains(SECRET));
        let hello = AgentMsg::Hello { secret: secret() };
        assert!(!format!("{hello:?}").contains(SECRET));
        assert!(!format!("{:?}", secret()).contains(SECRET));
    }

    #[test]
    fn secret_rules_and_comparison() {
        assert_eq!(Secret::parse("short"), Err(SecretError::TooShort));
        assert_eq!(
            Secret::parse("has a space in it!!"),
            Err(SecretError::Charset)
        );
        assert_eq!(
            Secret::parse("кириллица-кириллица"),
            Err(SecretError::Charset)
        );
        let parsed = Secret::parse(&format!("  {SECRET}\n")).unwrap();
        assert_eq!(parsed.expose(), SECRET);
        assert!(parsed.matches(SECRET.as_bytes()));
        assert!(!parsed.matches(&SECRET.as_bytes()[1..]));
        assert!(!parsed.matches(b"0123456789abcdef-secreT"));
        assert!(!parsed.matches(b""));
    }

    #[tokio::test]
    async fn lines_are_read_one_at_a_time() {
        let mut reader = tokio::io::BufReader::new(&b"{\"a\":1}\n{\"b\":2}\ntail"[..]);
        let mut buf = Vec::new();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"a\":1}\n");
        buf.clear();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"b\":2}\n");
        buf.clear();
        // A line cut by EOF is a closed connection, not a message.
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::Closed)
        );
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::Closed)
        );
    }

    #[tokio::test]
    async fn a_cancelled_read_keeps_its_partial_line() {
        let (mut write, read) = tokio::io::duplex(64);
        let mut reader = tokio::io::BufReader::new(read);
        let mut buf = Vec::new();

        write.write_all(b"{\"a\":").await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                read_line(&mut reader, &mut buf),
            )
            .await
            .is_err()
        );
        assert_eq!(buf, b"{\"a\":");

        write.write_all(b"1}\n").await.unwrap();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"a\":1}\n");
    }

    #[tokio::test]
    async fn an_endless_line_stops_at_the_limit() {
        // `repeat` never ends and never sends a newline.
        let mut reader = tokio::io::BufReader::new(tokio::io::repeat(b'a'));
        let mut buf = Vec::new();
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::TooLong)
        );
        assert_eq!(buf.len(), MAX_LINE);
        assert!(buf.capacity() <= 2 * MAX_LINE, "{}", buf.capacity());

        let mut exact = vec![b'a'; MAX_LINE - 1];
        exact.push(b'\n');
        let mut reader = tokio::io::BufReader::new(&exact[..]);
        buf.clear();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf.len(), MAX_LINE);
    }

    #[test]
    fn hook_posts_round_trip_and_keep_their_event_id() {
        for event in hook_samples() {
            let post = HookPost::new(
                "box".into(),
                "s1".into(),
                "/w".into(),
                "/w/s1.jsonl".into(),
                event,
            );
            let body = serde_json::to_vec(&post).unwrap();
            let decoded = decode_hook(&body).unwrap();
            assert_eq!(decoded.event_id, post.event_id);
            assert_eq!(decoded, post);
            // No list: no key on the wire (an older hub never sees one).
            assert!(
                !String::from_utf8(body)
                    .unwrap()
                    .contains("live_claude_pids")
            );
            let mut listed = post.clone();
            listed.live_claude_pids = Some(vec![10, 42]);
            let body = serde_json::to_vec(&listed).unwrap();
            assert_eq!(decode_hook(&body).unwrap(), listed);
        }
    }

    #[test]
    fn bad_hook_bodies_are_typed_errors() {
        let id = EventId::new();
        let body = |v: Value| serde_json::to_vec(&v).unwrap();
        let base = |event: Value| json!({ "v": 1, "event_id": id.as_str(), "host": "h", "session_id": "s", "event": event });
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "session_end" })))).map(|p| p.event),
            Ok(HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            })
        );
        let mut v2 = base(json!({ "type": "stop" }));
        v2["v"] = json!(2);
        assert_eq!(decode_hook(&body(v2)), Err(WireError::Version));
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "pre_compact" })))),
            Err(WireError::UnknownKind)
        );
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "subagent_start" })))),
            Err(WireError::Malformed)
        );
        let mut bad_id = base(json!({ "type": "stop" }));
        bad_id["event_id"] = json!("session-5e551017");
        assert_eq!(decode_hook(&body(bad_id)), Err(WireError::Malformed));
        assert_eq!(decode_hook(b"{"), Err(WireError::Malformed));
    }

    #[test]
    fn permission_posts_round_trip_and_check_the_version() {
        let post = PermissionPost {
            v: VERSION,
            host: "box".into(),
            session_id: "s".into(),
            tool_name: "Bash".into(),
            description: "d".into(),
            input_preview: "{}".into(),
        };
        let body = serde_json::to_vec(&post).unwrap();
        assert_eq!(decode_permission(&body), Ok(post));
        let short = br#"{"v":1,"host":"h","session_id":"s","tool_name":"Bash"}"#;
        assert_eq!(
            decode_permission(short).map(|p| p.description),
            Ok(String::new())
        );
        let v2 = br#"{"v":2,"host":"h","session_id":"s","tool_name":"Bash"}"#;
        assert_eq!(decode_permission(v2), Err(WireError::Version));
        assert_eq!(
            decode_permission(br#"{"v":1,"host":"h"}"#),
            Err(WireError::Malformed)
        );
        let answer = serde_json::to_string(&PermissionAnswer {
            behavior: Behavior::Deny,
        })
        .unwrap();
        assert_eq!(answer, r#"{"behavior":"deny"}"#);
    }

    #[test]
    fn event_ids_are_fresh_hex() {
        let ids: HashSet<EventId> = (0..10_000).map(|_| EventId::new()).collect();
        assert_eq!(ids.len(), 10_000);
        for id in ids.iter().take(10) {
            assert!(EventId::try_from(id.as_str().to_owned()).is_ok());
        }
        assert!(EventId::try_from("A".repeat(32)).is_err());
        assert!(EventId::try_from("a".repeat(31)).is_err());
    }
}

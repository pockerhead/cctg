//! The Claude Code channel surface of `cctg agent`: MCP over stdio, one
//! JSON-RPC 2.0 object per line, hand-rolled on `serde_json`.
//!
//! Implemented: `initialize` (channel, permission relay and tools
//! capabilities), `notifications/initialized`, `ping`, `tools/list`,
//! `tools/call` (`reply`, and `send_file`, whose answer the agent loop
//! writes once the hub answered: see [`Server::take_file_calls`]), the
//! outgoing `notifications/tools/list_changed` ([`tools_changed`]), the incoming
//! `notifications/claude/channel/permission_request` and the outgoing
//! `notifications/claude/channel` and `notifications/claude/channel/permission`.
//! Every other request answers method-not-found; other notifications are
//! ignored. Only JSON-RPC 2.0 (`"jsonrpc": "2.0"`, structured `params`) is
//! served; anything else is an invalid request.
//!
//! [`Server`] is pure state: it turns one input into the lines to write and
//! the messages to queue for the hub. Nothing here touches stdout; the agent
//! loop owns it. Error texts are fixed: input from either side is never
//! echoed back or logged.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::agent::LinkEvent;
use crate::wire::{AgentMsg, Behavior, HubMsg, PermissionRequest};

pub const SERVER_NAME: &str = "cctg";
/// The MCP revision answered when the client asks for one we do not know
/// (Claude Code 2.1.280 asks for this one).
pub const LATEST_PROTOCOL: &str = "2025-11-25";
/// Revisions echoed back as asked: everything used here (tools, experimental
/// capabilities, instructions) means the same in each of them.
pub const SUPPORTED_PROTOCOLS: [&str; 4] =
    [LATEST_PROTOCOL, "2025-06-18", "2025-03-26", "2024-11-05"];
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// Channel notifications held until Claude Code sends `initialized`.
pub const MAX_HELD: usize = 64;
/// Recently relayed permission ids kept to suppress a repeated request.
/// Claude Code never says when a prompt was answered in the terminal, so
/// this is a window, not a set of open requests: the oldest id falls out.
pub const RECENT_PERMISSIONS: usize = 256;
pub const REPLY_TOOL: &str = "reply";
/// Sends a file of this machine to the topic (TASK-032).
pub const SEND_FILE_TOOL: &str = "send_file";
/// Longest `send_file` caption passed on, in bytes; the hub cuts it to what
/// Telegram shows.
pub const MAX_CAPTION: usize = 4 << 10;
/// Longest reply text sent to the hub, in bytes. Even with every character
/// `\u`-escaped the link line stays under `wire::MAX_LINE`.
pub const MAX_REPLY: usize = 128 << 10;
/// Longest permission request field, in bytes; three of them, escaped, still
/// fit one link line. Claude Code already cuts each field to 3,500 code points.
pub const MAX_PERMISSION_FIELD: usize = 32 << 10;

pub const INSTRUCTIONS: &str = "Messages from the user's Telegram topic for this session arrive as \
<channel source=\"cctg\" ...>. The user reads Telegram, not this terminal, and everything you do \
here shows up in the Telegram topic automatically: the text you write, your visible thinking, one \
line per tool call and the final answer of each turn. Just work and answer normally. You do not \
need this server's `reply` tool (`mcp__<server>__reply`, normally `mcp__cctg__reply`): it is kept \
only for compatibility, and a message sent through it repeats what the user already sees. \
The topic may be shared by a team whose members are all equal users: then each message, and each \
`---`-separated part of a message made of several, starts with its author's name and a colon, and \
the tag has a `from_name` attribute when one person wrote all of it. \
If the tag has a `target_agent` attribute, the message is for that subagent, running or finished: \
forward it with SendMessage to that agent instead of acting on it yourself. Tool permission prompts are relayed to Telegram by Claude Code itself; \
never ask for permissions through `reply`. A tag with a `file_path` attribute brings a file the user sent \
(a photo, a document, a voice message...): it is saved on this machine at that path; open it with your \
tools when it matters. To give the user a file of this machine, call this server's `send_file` tool \
(normally `mcp__cctg__send_file`) with its path and a short caption saying what the file is: \
pictures arrive as photos, anything else as a document, 50 MB at most.";

/// Why this agent has no hub link. Shown to Claude when it calls `reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoHub {
    /// `claude -p`: Claude Code never starts channels in headless runs.
    Headless,
    /// `CLAUDE_CODE_SESSION_ID` is missing: not spawned by Claude Code.
    NoSession,
    /// The device config has no usable `CCTG_HUB_SECRET`.
    NoConfig,
}

impl NoHub {
    pub fn text(self) -> &'static str {
        match self {
            Self::Headless => {
                "cctg: this is a headless (claude -p) run; it has no Telegram channel"
            }
            Self::NoSession => "cctg: no Claude Code session id; the Telegram channel is off",
            Self::NoConfig => {
                "cctg: the hub is not configured on this device (~/.cctg/device.env); nothing was sent"
            }
        }
    }
}

/// Where hub-bound messages go.
#[derive(Debug)]
pub enum Hub {
    Off(NoHub),
    Link(mpsc::Sender<AgentMsg>),
}

pub struct Server {
    hub: Hub,
    hub_up: bool,
    initialized: bool,
    held: VecDeque<Vec<u8>>,
    recent_permissions: VecDeque<String>,
    file_calls: Vec<FileCall>,
}

/// A `send_file` call waiting for the agent loop; its answer is
/// [`tool_answer`] for `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileCall {
    pub id: Value,
    pub path: String,
    pub caption: Option<String>,
}

impl Server {
    pub fn new(hub: Hub) -> Self {
        Self {
            hub,
            hub_up: false,
            initialized: false,
            held: VecDeque::new(),
            recent_permissions: VecDeque::new(),
            file_calls: Vec::new(),
        }
    }

    /// The `send_file` calls since the last take, oldest first. Each still
    /// needs its answer.
    pub fn take_file_calls(&mut self) -> Vec<FileCall> {
        std::mem::take(&mut self.file_calls)
    }

    /// A server for a session whose channel an earlier worker already set up
    /// (TASK-040): Claude Code sends no second `initialize`, so channel
    /// notifications go out at once.
    pub fn initialized(mut self) -> Self {
        self.initialized = true;
        self
    }

    /// One line from Claude Code (newline optional). Returns the lines to
    /// write, each a complete JSON object followed by `\n`.
    pub fn on_line(&mut self, line: &[u8]) -> Vec<Vec<u8>> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return vec![error(&Value::Null, PARSE_ERROR, "Parse error")];
        };
        let Value::Object(msg) = value else {
            // Batches are not part of MCP since 2025-06-18.
            return vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")];
        };
        // A response to a request of ours: we send none, so drop it.
        if !msg.contains_key("method")
            && msg.contains_key("id")
            && (msg.contains_key("result") || msg.contains_key("error"))
        {
            return Vec::new();
        }
        // Not JSON-RPC 2.0: its id is not ours to trust.
        if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")];
        }
        let id = msg.get("id").filter(|id| id.is_string() || id.is_number());
        let params = msg.get("params");
        let structured = params.is_none_or(|params| params.is_object() || params.is_array());
        match (msg.get("method"), msg.get("id")) {
            (Some(Value::String(method)), None) if structured => {
                self.on_notification(method, params);
                self.flush_if_ready(method)
            }
            (Some(Value::String(method)), Some(_)) if structured => match id {
                Some(id) => self.on_request(id, method, params).into_iter().collect(),
                None => vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")],
            },
            _ => vec![error(
                id.unwrap_or(&Value::Null),
                INVALID_REQUEST,
                "Invalid Request",
            )],
        }
    }

    /// A line longer than the reader accepts was skipped.
    pub fn on_oversized_line(&mut self) -> Vec<Vec<u8>> {
        warn!("oversized line from Claude Code skipped");
        vec![error(&Value::Null, PARSE_ERROR, "Parse error")]
    }

    /// Link state and hub messages.
    pub fn on_link(&mut self, event: LinkEvent) -> Vec<Vec<u8>> {
        match event {
            LinkEvent::Up { .. } => {
                self.hub_up = true;
                Vec::new()
            }
            LinkEvent::Down => {
                self.hub_up = false;
                Vec::new()
            }
            LinkEvent::Message(HubMsg::Inbound { content, meta }) => {
                let line = notification(
                    "notifications/claude/channel",
                    json!({ "content": content, "meta": channel_meta(meta) }),
                );
                self.emit(line)
            }
            LinkEvent::Message(HubMsg::PermissionVerdict {
                request_id,
                behavior,
                ..
            }) => {
                // Claude Code applies a verdict only to its own pending id,
                // so every well-formed one is passed on.
                if !is_request_id(&request_id) {
                    debug!("verdict without a valid request id; dropped");
                    return Vec::new();
                }
                self.recent_permissions.retain(|id| *id != request_id);
                let behavior = match behavior {
                    Behavior::Allow => "allow",
                    Behavior::Deny => "deny",
                };
                let line = notification(
                    "notifications/claude/channel/permission",
                    json!({ "request_id": request_id, "behavior": behavior }),
                );
                self.emit(line)
            }
            // Transcript and session reads, console keys and commands,
            // updates and files are the agent loop's, not the channel's;
            // pings and `bound` end in the link task.
            LinkEvent::Message(
                HubMsg::Registered { .. }
                | HubMsg::Ping
                | HubMsg::Bound { .. }
                | HubMsg::Rejected { .. }
                | HubMsg::TranscriptRead { .. }
                | HubMsg::SessionRead { .. }
                | HubMsg::ConsoleKey { .. }
                | HubMsg::ConsoleCommand { .. }
                | HubMsg::Update { .. }
                | HubMsg::Released { .. }
                | HubMsg::FileStart { .. }
                | HubMsg::FileChunk(_)
                | HubMsg::FileAnswer { .. },
            ) => Vec::new(),
        }
    }

    fn emit(&mut self, line: Vec<u8>) -> Vec<Vec<u8>> {
        if self.initialized {
            return vec![line];
        }
        if self.held.len() == MAX_HELD {
            self.held.pop_front();
            warn!("channel notification dropped before initialization");
        }
        self.held.push_back(line);
        Vec::new()
    }

    fn flush_if_ready(&mut self, method: &str) -> Vec<Vec<u8>> {
        if method == "notifications/initialized" {
            self.held.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    fn on_notification(&mut self, method: &str, params: Option<&Value>) {
        match method {
            "notifications/initialized" => self.initialized = true,
            "notifications/claude/channel/permission_request" => {
                self.on_permission_request(params);
            }
            _ => debug!("notification ignored"),
        }
    }

    fn on_permission_request(&mut self, params: Option<&Value>) {
        let field = |name: &str| {
            params
                .and_then(|params| params.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let Some(request_id) = field("request_id").filter(|id| is_request_id(id)) else {
            warn!("permission request without a valid request id; ignored");
            return;
        };
        let Hub::Link(outbox) = &self.hub else {
            return;
        };
        if self.recent_permissions.contains(&request_id) {
            debug!("permission request relayed recently; not relayed again");
            return;
        }
        let request = PermissionRequest {
            request_id: request_id.clone(),
            tool_name: cap(field("tool_name").unwrap_or_default(), MAX_PERMISSION_FIELD),
            description: cap(
                field("description").unwrap_or_default(),
                MAX_PERMISSION_FIELD,
            ),
            input_preview: cap(
                field("input_preview").unwrap_or_default(),
                MAX_PERMISSION_FIELD,
            ),
        };
        if outbox
            .try_send(AgentMsg::PermissionRequest(request))
            .is_err()
        {
            warn!("hub queue full; permission request not relayed");
            return;
        }
        if self.recent_permissions.len() == RECENT_PERMISSIONS {
            self.recent_permissions.pop_front();
        }
        self.recent_permissions.push_back(request_id);
    }

    /// The answer to a request; `None` only for a `send_file` call, which
    /// the agent loop answers later.
    fn on_request(&mut self, id: &Value, method: &str, params: Option<&Value>) -> Option<Vec<u8>> {
        let line = match method {
            "initialize" => {
                let Some(asked) = params
                    .and_then(|params| params.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .filter(|version| !version.is_empty())
                else {
                    return Some(error(
                        id,
                        INVALID_PARAMS,
                        "initialize needs a protocolVersion string",
                    ));
                };
                let version = if SUPPORTED_PROTOCOLS.contains(&asked) {
                    asked
                } else {
                    LATEST_PROTOCOL
                };
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {
                            // A worker that took over after an update may
                            // offer more tools than the one Claude Code met.
                            "tools": { "listChanged": true },
                            "experimental": {
                                "claude/channel": {},
                                "claude/channel/permission": {},
                            },
                        },
                        "serverInfo": {
                            "name": SERVER_NAME,
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": [reply_tool(), send_file_tool()] })),
            "tools/call" => {
                let name = params
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str);
                let arguments = params.and_then(|params| params.get("arguments"));
                match name {
                    Some(REPLY_TOOL) => result(id, self.reply(arguments)),
                    Some(SEND_FILE_TOOL) => return self.send_file(id, arguments),
                    Some(_) => error(id, INVALID_PARAMS, "Unknown tool"),
                    None => error(id, INVALID_PARAMS, "tools/call needs a tool name"),
                }
            }
            _ => error(id, METHOD_NOT_FOUND, "Method not found"),
        };
        Some(line)
    }

    /// The `send_file` tool: a well-formed call waits for the agent loop
    /// ([`Self::take_file_calls`]); input problems and a missing hub are
    /// answered at once as tool errors.
    fn send_file(&mut self, id: &Value, arguments: Option<&Value>) -> Option<Vec<u8>> {
        let answer = |text: &str| Some(tool_answer(id, text, true));
        let path = arguments
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty());
        let Some(path) = path else {
            return answer("send_file needs a non-empty `path` string");
        };
        let caption = match arguments.and_then(|arguments| arguments.get("caption")) {
            None | Some(Value::Null) => None,
            Some(Value::String(caption)) if caption.trim().is_empty() => None,
            Some(Value::String(caption)) => Some(cap(caption.clone(), MAX_CAPTION)),
            Some(_) => return answer("`caption` must be a string"),
        };
        if let Hub::Off(reason) = &self.hub {
            return answer(reason.text());
        }
        self.file_calls.push(FileCall {
            id: id.clone(),
            path: path.to_owned(),
            caption,
        });
        None
    }

    /// The `reply` tool. Input problems are tool errors (`isError`), so
    /// Claude can correct them; only protocol problems are JSON-RPC errors.
    fn reply(&mut self, arguments: Option<&Value>) -> Value {
        let text = arguments
            .and_then(|arguments| arguments.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty());
        let Some(text) = text else {
            return tool_result("reply needs a non-empty `text` string", true);
        };
        let outbox = match &self.hub {
            Hub::Off(reason) => return tool_result(reason.text(), true),
            Hub::Link(outbox) => outbox,
        };
        let reply = AgentMsg::Reply {
            text: cap(text.to_owned(), MAX_REPLY),
        };
        match outbox.try_send(reply) {
            Ok(()) if self.hub_up => {
                tool_result("Sent to the Telegram topic of this session.", false)
            }
            Ok(()) => tool_result(
                "The cctg hub is not reachable right now; the reply is queued and goes out when the link is back.",
                false,
            ),
            Err(_) => tool_result("The cctg hub queue is full; the reply was not sent.", true),
        }
    }
}

/// Cuts on a char boundary and marks the cut with an ellipsis.
fn cap(mut text: String, max: usize) -> String {
    if text.len() > max {
        let cut = text.floor_char_boundary(max - '\u{2026}'.len_utf8());
        text.truncate(cut);
        text.push('\u{2026}');
    }
    text
}

fn send_file_tool() -> Value {
    json!({
        "name": SEND_FILE_TOOL,
        "description": "Sends a file of this machine to the user's Telegram topic of this \
            session: a JPEG, PNG or WebP picture of up to 10 MB arrives as a photo, anything \
            else as a document; 50 MB at most. Use it when the user should get the file itself \
            (a screenshot, a report, a build artifact), not for text: what you write reaches \
            the topic anyway. Answers once Telegram took the file.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file: an absolute path, or one relative to the session's working folder.",
                },
                "caption": {
                    "type": "string",
                    "description": "Always give one: a short text shown with the file, what it is and why you send it (Telegram shows at most 1024 characters). Without it the file name is shown.",
                },
            },
            "required": ["path"],
            "additionalProperties": false,
        },
    })
}

fn reply_tool() -> Value {
    json!({
        "name": REPLY_TOOL,
        "description": "Not needed: everything this Claude Code session writes, its tool \
            calls and the final answer of each turn reach its Telegram topic automatically. \
            Kept for compatibility; sends one more plain-text message to that topic.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Message text, plain text." },
            },
            "required": ["text"],
            "additionalProperties": false,
        },
    })
}

/// Keeps only keys Claude Code accepts (`[A-Za-z0-9_]+`); other keys are
/// dropped, never renamed. Values are passed through unchanged.
pub fn channel_meta(meta: BTreeMap<String, String>) -> Map<String, Value> {
    let total = meta.len();
    let kept: Map<String, Value> = meta
        .into_iter()
        .filter(|(key, _)| is_meta_key(key))
        .map(|(key, value)| (key, Value::String(value)))
        .collect();
    if kept.len() != total {
        debug!(dropped = total - kept.len(), "invalid meta keys dropped");
    }
    kept
}

pub fn is_meta_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Five lowercase letters without `l`, as Claude Code issues them.
pub fn is_request_id(id: &str) -> bool {
    id.len() == 5
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() && byte != b'l')
}

fn line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("a JSON value always serializes");
    line.push(b'\n');
    line
}

fn result(id: &Value, result: Value) -> Vec<u8> {
    line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: &Value, code: i64, message: &str) -> Vec<u8> {
    line(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

fn notification(method: &str, params: Value) -> Vec<u8> {
    line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The answer line of a tool call `id`.
pub fn tool_answer(id: &Value, text: &str, is_error: bool) -> Vec<u8> {
    result(id, tool_result(text, is_error))
}

/// Tells Claude Code to list the tools again: a worker that took over from
/// an older one after an update may offer tools the older one did not.
pub fn tools_changed() -> Vec<u8> {
    notification("notifications/tools/list_changed", json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[Vec<u8>]) -> Vec<Value> {
        lines
            .iter()
            .map(|line| {
                assert!(line.ends_with(b"\n"));
                assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
                let value: Value = serde_json::from_slice(line).expect("one JSON object");
                assert!(value.is_object());
                value
            })
            .collect()
    }

    fn one(server: &mut Server, input: &str) -> Value {
        let out = parse(&server.on_line(input.as_bytes()));
        assert_eq!(out.len(), 1, "{input}");
        out.into_iter().next().unwrap()
    }

    fn linked() -> (Server, mpsc::Receiver<AgentMsg>) {
        let (tx, rx) = mpsc::channel(4);
        (Server::new(Hub::Link(tx)), rx)
    }

    fn init(server: &mut Server) {
        one(
            server,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        );
        assert!(
            server
                .on_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_empty()
        );
    }

    #[test]
    fn initialize_declares_the_channel() {
        let (mut server, _rx) = linked();
        let answer = one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#,
        );
        assert_eq!(answer["id"], 0);
        let result = &answer["result"];
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert_eq!(
            result["capabilities"]["tools"],
            json!({ "listChanged": true })
        );
        assert_eq!(
            result["capabilities"]["experimental"],
            json!({ "claude/channel": {}, "claude/channel/permission": {} })
        );
        assert_eq!(result["serverInfo"]["name"], "cctg");
        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("`mcp__<server>__reply`"));
        assert!(instructions.contains("`mcp__cctg__reply`"));
        assert!(instructions.contains("shows up in the Telegram topic automatically"));
        assert!(instructions.contains("visible thinking"));
        assert!(instructions.contains("one line per tool call"));
        assert!(instructions.contains("You do not need this server's `reply` tool"));
        assert!(instructions.contains("kept only for compatibility"));
        // TASK-025: `reply` is no longer offered for progress messages.
        assert!(!instructions.contains("only for extra messages"));
        assert!(!instructions.contains("ToolSearch"));
        assert!(!instructions.contains("answer each such message with"));
        assert!(instructions.contains("SendMessage"));
        assert!(instructions.contains("that subagent, running or finished"));
        assert!(instructions.contains("never ask for permissions through `reply`"));
        assert!(instructions.contains("`file_path` attribute"));
        assert!(instructions.contains("`mcp__cctg__send_file`"));
        assert!(instructions.contains("a short caption saying what the file is"));
    }

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        let (mut server, _rx) = linked();
        // A revision we support is echoed; any other gets our latest.
        for (asked, answered) in [
            ("2025-11-25", "2025-11-25"),
            ("2025-06-18", "2025-06-18"),
            ("2025-03-26", "2025-03-26"),
            ("2024-11-05", "2024-11-05"),
            ("9999-99-99", LATEST_PROTOCOL),
            ("2024-10-07", LATEST_PROTOCOL),
        ] {
            let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":asked}});
            let answer = one(&mut server, &request.to_string());
            assert_eq!(answer["result"]["protocolVersion"], answered, "{asked}");
        }
        // No usable version: invalid params, never a guess.
        for input in [
            r#"{"jsonrpc":"2.0","id":"x","method":"initialize"}"#,
            r#"{"jsonrpc":"2.0","id":"x","method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":""}}"#,
            r#"{"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":20251125}}"#,
        ] {
            let answer = one(&mut server, input);
            assert_eq!(answer["id"], "x", "{input}");
            assert_eq!(answer["error"]["code"], INVALID_PARAMS, "{input}");
            assert!(answer.get("result").is_none(), "{input}");
        }
    }

    #[test]
    fn ping_answers_an_empty_result_at_any_time() {
        let (mut server, _rx) = linked();
        let answer = one(&mut server, r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#);
        assert_eq!(answer["id"], "p");
        assert_eq!(answer["result"], json!({}));
        init(&mut server);
        let answer = one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":9,"method":"ping","params":{}}"#,
        );
        assert_eq!(answer["result"], json!({}));
        let answer = one(&mut server, r#"{"jsonrpc":"2.0","id":10,"method":"pong"}"#);
        assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn only_json_rpc_2_0_with_structured_params_is_served() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        for (input, id) in [
            // Not JSON-RPC 2.0: the id cannot be trusted.
            (r#"{"id":1,"method":"tools/list"}"#, Value::Null),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"tools/list"}"#,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":2.0,"id":1,"method":"tools/list"}"#,
                Value::Null,
            ),
            (r#"{"jsonrpc":null,"id":1,"method":"ping"}"#, Value::Null),
            (r#"{"method":"notifications/initialized"}"#, Value::Null),
            (
                r#"{"method":"notifications/claude/channel/permission_request","params":{"request_id":"abcde"}}"#,
                Value::Null,
            ),
            // JSON-RPC 2.0, but params must be an object or an array.
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":5}"#,
                json!(2),
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":"x"}"#,
                json!(2),
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":true}"#,
                json!(2),
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":null}"#,
                json!(2),
            ),
            (
                r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":"abcde"}"#,
                Value::Null,
            ),
        ] {
            let answer = one(&mut server, input);
            assert_eq!(answer["id"], id, "{input}");
            assert_eq!(answer["error"]["code"], INVALID_REQUEST, "{input}");
        }
        assert!(rx.try_recv().is_err(), "nothing reached the hub");
        // The next valid request is served as usual.
        let answer = one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        );
        assert_eq!(answer["result"]["tools"][0]["name"], "reply");
    }

    #[test]
    fn a_notification_without_json_rpc_2_0_does_not_initialize() {
        let (mut server, _rx) = linked();
        one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        );
        one(&mut server, r#"{"method":"notifications/initialized"}"#);
        let inbound = LinkEvent::Message(HubMsg::Inbound {
            content: "held".into(),
            meta: BTreeMap::new(),
        });
        assert!(server.on_link(inbound).is_empty(), "still held");
    }

    #[test]
    fn a_duplicate_permission_request_is_relayed_once() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        let request = br#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"abcde","tool_name":"Bash","description":"d","input_preview":"p"}}"#;
        assert!(server.on_line(request).is_empty());
        assert!(server.on_line(request).is_empty());
        assert!(matches!(rx.try_recv(), Ok(AgentMsg::PermissionRequest(_))));
        assert!(rx.try_recv().is_err(), "the duplicate is not relayed");
        let verdict = LinkEvent::Message(HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Allow,
            verdict_id: None,
        });
        assert_eq!(server.on_link(verdict).len(), 1);
        // After its verdict, the same id may be asked again.
        assert!(server.on_line(request).is_empty());
        assert!(matches!(rx.try_recv(), Ok(AgentMsg::PermissionRequest(_))));
    }

    fn nth_request_id(mut n: usize) -> String {
        let alphabet = b"abcdefghijkmnopqrstuvwxyz";
        let mut id = [alphabet[0]; 5];
        for byte in id.iter_mut().rev() {
            *byte = alphabet[n % alphabet.len()];
            n /= alphabet.len();
        }
        String::from_utf8(id.to_vec()).unwrap()
    }

    fn permission_request(id: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel/permission_request",
            "params": { "request_id": id, "tool_name": "Bash", "description": "d", "input_preview": "{}" }
        })
        .to_string()
    }

    fn relayed(rx: &mut mpsc::Receiver<AgentMsg>) -> Vec<String> {
        let mut ids = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                AgentMsg::PermissionRequest(request) => ids.push(request.request_id),
                other => panic!("unexpected {other:?}"),
            }
        }
        ids
    }

    fn verdict(id: &str) -> LinkEvent {
        LinkEvent::Message(HubMsg::PermissionVerdict {
            request_id: id.into(),
            behavior: Behavior::Deny,
            verdict_id: None,
        })
    }

    /// QA TASK-013 `perm_leak.py`: every prompt is answered in the terminal,
    /// so no verdict ever comes back. Relay must never stop.
    #[test]
    fn prompts_answered_in_the_terminal_never_stop_the_relay() {
        let total = RECENT_PERMISSIONS + 70;
        let (tx, mut rx) = mpsc::channel(total);
        let mut server = Server::new(Hub::Link(tx));
        init(&mut server);
        for n in 0..total {
            assert!(
                server
                    .on_line(permission_request(&nth_request_id(n)).as_bytes())
                    .is_empty()
            );
        }
        let expected: Vec<String> = (0..total).map(nth_request_id).collect();
        assert_eq!(relayed(&mut rx), expected, "every request reached the hub");
    }

    #[test]
    fn a_recent_duplicate_is_suppressed_and_an_evicted_verdict_still_passes() {
        let (tx, mut rx) = mpsc::channel(RECENT_PERMISSIONS + 4);
        let mut server = Server::new(Hub::Link(tx));
        init(&mut server);
        let oldest = nth_request_id(0);
        for n in 0..RECENT_PERMISSIONS {
            server.on_line(permission_request(&nth_request_id(n)).as_bytes());
        }
        assert_eq!(relayed(&mut rx).len(), RECENT_PERMISSIONS);
        // Still inside the window: not relayed twice.
        server.on_line(permission_request(&oldest).as_bytes());
        assert!(
            relayed(&mut rx).is_empty(),
            "a recent duplicate is not relayed"
        );

        // One more id pushes the oldest out of the window.
        server.on_line(permission_request(&nth_request_id(RECENT_PERMISSIONS)).as_bytes());
        assert_eq!(relayed(&mut rx).len(), 1);

        // Its verdict still reaches Claude Code, exactly once per verdict.
        let out = parse(&server.on_link(verdict(&oldest)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "notifications/claude/channel/permission");
        assert_eq!(out[0]["params"]["request_id"], oldest.as_str());
        assert_eq!(out[0]["params"]["behavior"], "deny");

        // Out of the window, the same id is a new request again.
        server.on_line(permission_request(&oldest).as_bytes());
        assert_eq!(relayed(&mut rx), vec![oldest]);
    }

    #[test]
    fn a_verdict_is_forwarded_without_a_relayed_request_but_never_malformed() {
        let (mut server, _rx) = linked();
        init(&mut server);
        assert_eq!(server.on_link(verdict("qwert")).len(), 1);
        for bad in ["", "abcdl", "ABCDE", "abcd", "abcdef", "ab\"cd"] {
            assert!(server.on_link(verdict(bad)).is_empty(), "{bad}");
        }
    }

    #[test]
    fn tools_list_offers_reply_and_send_file() {
        let (mut server, _rx) = linked();
        init(&mut server);
        let answer = one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        let tools = answer["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1]["name"], "send_file");
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["path"]));
        let description = tools[1]["description"].as_str().unwrap();
        assert!(description.contains("as a photo") && description.contains("50 MB"));
        // TASK-051: a caption is asked for; the name stands in without one.
        let caption = tools[1]["inputSchema"]["properties"]["caption"]["description"]
            .as_str()
            .unwrap();
        assert!(caption.starts_with("Always give one") && caption.contains("file name"));
        assert_eq!(tools[0]["name"], "reply");
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["text"]));
        let description = tools[0]["description"].as_str().unwrap();
        assert!(description.starts_with("Not needed:"));
        assert!(description.contains("automatically"));
        assert!(description.contains("Kept for compatibility"));
    }

    #[test]
    fn reply_goes_to_the_hub() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reply","arguments":{"text":"done ✓"}}}"#;
        let answer = one(&mut server, call);
        assert_eq!(answer["result"]["isError"], false);
        assert!(
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("queued")
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            AgentMsg::Reply {
                text: "done ✓".into()
            }
        );
        server.on_link(LinkEvent::Up { files: true });
        let answer = one(&mut server, call);
        assert!(
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("Sent")
        );
    }

    #[test]
    fn a_send_file_call_waits_for_the_loop_and_bad_ones_are_answered_at_once() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        let call = |id: u32, arguments: Value| {
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"send_file","arguments":arguments}})
                .to_string()
        };
        let lines = server
            .on_line(call(4, json!({ "path": "C:/x/shot.png", "caption": "look" })).as_bytes());
        assert!(lines.is_empty(), "answered later by the loop");
        let blank_caption = call(5, json!({ "path": "rel.txt", "caption": "  " }));
        assert!(server.on_line(blank_caption.as_bytes()).is_empty());
        assert_eq!(
            server.take_file_calls(),
            [
                FileCall {
                    id: json!(4),
                    path: "C:/x/shot.png".into(),
                    caption: Some("look".into()),
                },
                FileCall {
                    id: json!(5),
                    path: "rel.txt".into(),
                    caption: None,
                },
            ]
        );
        assert!(server.take_file_calls().is_empty());
        for arguments in [
            json!({}),
            json!({ "path": " " }),
            json!({ "path": 7 }),
            json!({ "path": "a", "caption": 1 }),
        ] {
            let answer = one(&mut server, &call(6, arguments.clone()));
            assert_eq!(answer["id"], 6, "{arguments}");
            assert_eq!(answer["result"]["isError"], true, "{arguments}");
        }
        assert!(server.take_file_calls().is_empty());
        assert!(rx.try_recv().is_err(), "nothing reached the hub");
        // Without a hub it says why at once.
        let mut off = Server::new(Hub::Off(NoHub::Headless));
        init(&mut off);
        let answer = one(&mut off, &call(7, json!({ "path": "a.txt" })));
        assert_eq!(
            answer["result"]["content"][0]["text"],
            NoHub::Headless.text()
        );
        assert!(off.take_file_calls().is_empty());
        let long = "\u{1}".repeat(MAX_CAPTION * 2);
        assert!(
            server
                .on_line(call(8, json!({ "path": "a", "caption": long })).as_bytes())
                .is_empty()
        );
        let capped = server.take_file_calls().remove(0).caption.unwrap();
        assert!(capped.len() <= MAX_CAPTION && capped.ends_with('\u{2026}'));
        let answer: Value =
            serde_json::from_slice(&tool_answer(&json!("x"), "done", false)).unwrap();
        assert_eq!(answer["id"], "x");
        assert_eq!(answer["result"]["isError"], false);
        let changed: Value = serde_json::from_slice(&tools_changed()).unwrap();
        assert_eq!(changed["method"], "notifications/tools/list_changed");
        assert!(changed.get("id").is_none());
    }

    #[test]
    fn bad_calls_are_answered_not_fatal() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        for (input, code) in [
            (
                r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#,
                METHOD_NOT_FOUND,
            ),
            (
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"other"}}"#,
                INVALID_PARAMS,
            ),
            (
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call"}"#,
                INVALID_PARAMS,
            ),
        ] {
            let answer = one(&mut server, input);
            assert_eq!(answer["id"], 3, "{input}");
            assert_eq!(answer["error"]["code"], code, "{input}");
        }
        for input in [
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"reply"}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"reply","arguments":{"text":"  "}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"reply","arguments":{"text":7}}}"#,
        ] {
            let answer = one(&mut server, input);
            assert_eq!(answer["result"]["isError"], true, "{input}");
        }
        assert!(rx.try_recv().is_err(), "nothing reached the hub");
    }

    #[test]
    fn broken_input_gets_controlled_answers() {
        let (mut server, _rx) = linked();
        for (input, id, code) in [
            ("not json", Value::Null, PARSE_ERROR),
            ("{\"jsonrpc\":\"2.0\",\"id\":1,", Value::Null, PARSE_ERROR),
            ("[]", Value::Null, INVALID_REQUEST),
            ("42", Value::Null, INVALID_REQUEST),
            (
                r#"{"jsonrpc":"2.0","id":{"a":1},"method":"tools/list"}"#,
                Value::Null,
                INVALID_REQUEST,
            ),
            (
                r#"{"jsonrpc":"2.0","id":5,"method":7}"#,
                json!(5),
                INVALID_REQUEST,
            ),
            (r#"{"jsonrpc":"2.0","id":5}"#, json!(5), INVALID_REQUEST),
        ] {
            let answer = one(&mut server, input);
            assert_eq!(answer["id"], id, "{input}");
            assert_eq!(answer["error"]["code"], code, "{input}");
        }
        // Silence: blank lines, responses to us, unknown notifications.
        for input in [
            "",
            "  \r\n",
            r#"{"jsonrpc":"2.0","id":9,"result":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
        ] {
            assert!(server.on_line(input.as_bytes()).is_empty(), "{input}");
        }
        // Non-UTF-8 bytes and every prefix of a real request.
        let _ = parse(&server.on_line(b"\xff\xfe{"));
        let real = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"reply","arguments":{"text":"x"}}}"#;
        for cut in 0..real.len() {
            let _ = parse(&server.on_line(&real[..cut]));
        }
        assert_eq!(
            parse(&server.on_oversized_line())[0]["error"]["code"],
            PARSE_ERROR
        );
    }

    #[test]
    fn a_resumed_worker_sends_inbound_at_once_and_ignores_update_messages() {
        let (server, _rx) = linked();
        let mut server = server.initialized();
        let lines = server.on_link(LinkEvent::Message(HubMsg::Inbound {
            content: "after the hand-over".into(),
            meta: BTreeMap::new(),
        }));
        let out = parse(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "notifications/claude/channel");
        for msg in [
            HubMsg::Update {
                update_id: 1,
                release: None,
            },
            HubMsg::Released {
                update_id: 1,
                session_id: "s".into(),
            },
            HubMsg::FileAnswer {
                transfer_id: 1,
                outcome: crate::wire::FileOutcome::Sent,
            },
        ] {
            assert!(server.on_link(LinkEvent::Message(msg)).is_empty());
        }
    }

    #[test]
    fn inbound_waits_for_initialized_and_keeps_valid_meta_byte_for_byte() {
        let (mut server, _rx) = linked();
        let meta: BTreeMap<String, String> = [
            ("chat_id", "-1001"),
            ("target_agent", "a8c1bff86acd31609"),
            ("From2", "Артём \"quoted\" <tag> \n\t😀 \\ ok"),
            ("bad-key", "dropped"),
            ("", "dropped"),
            ("dotted.key", "dropped"),
            ("ключ", "dropped"),
            ("sp ace", "dropped"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let inbound = LinkEvent::Message(HubMsg::Inbound {
            content: "line one\nline two".into(),
            meta: meta.clone(),
        });
        assert!(server.on_link(inbound).is_empty(), "held before initialize");
        one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        );
        let flushed =
            parse(&server.on_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#));
        assert_eq!(flushed.len(), 1);
        let note = &flushed[0];
        assert_eq!(note["method"], "notifications/claude/channel");
        assert!(note.get("id").is_none());
        assert_eq!(note["params"]["content"], "line one\nline two");
        let got = note["params"]["meta"].as_object().unwrap();
        let keys: Vec<&str> = got.keys().map(String::as_str).collect();
        assert_eq!(keys, ["From2", "chat_id", "target_agent"]);
        for key in keys {
            assert_eq!(got[key].as_str().unwrap().as_bytes(), meta[key].as_bytes());
        }
        // After initialization, delivered at once.
        let later = server.on_link(LinkEvent::Message(HubMsg::Inbound {
            content: "now".into(),
            meta: BTreeMap::new(),
        }));
        assert_eq!(parse(&later)[0]["params"]["meta"], json!({}));
    }

    #[test]
    fn held_notifications_are_bounded() {
        let (mut server, _rx) = linked();
        for n in 0..MAX_HELD + 5 {
            server.on_link(LinkEvent::Message(HubMsg::Inbound {
                content: n.to_string(),
                meta: BTreeMap::new(),
            }));
        }
        let flushed =
            parse(&server.on_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#));
        assert_eq!(flushed.len(), MAX_HELD);
        assert_eq!(flushed[0]["params"]["content"], "5");
    }

    #[test]
    fn permission_relay_round_trip() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        let request = r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"tcmbm","tool_name":"Bash","description":"Create a file","input_preview":"{ \"command\": \"echo x > y\" }"}}"#;
        assert!(server.on_line(request.as_bytes()).is_empty());
        assert_eq!(
            rx.try_recv().unwrap(),
            AgentMsg::PermissionRequest(PermissionRequest {
                request_id: "tcmbm".into(),
                tool_name: "Bash".into(),
                description: "Create a file".into(),
                input_preview: "{ \"command\": \"echo x > y\" }".into(),
            })
        );
        // A verdict with a malformed id is dropped.
        let stray = server.on_link(LinkEvent::Message(HubMsg::PermissionVerdict {
            request_id: "zzzzl".into(),
            behavior: Behavior::Allow,
            verdict_id: None,
        }));
        assert!(stray.is_empty());
        let verdict = LinkEvent::Message(HubMsg::PermissionVerdict {
            request_id: "tcmbm".into(),
            behavior: Behavior::Deny,
            verdict_id: None,
        });
        let out = parse(&server.on_link(verdict));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["method"], "notifications/claude/channel/permission");
        assert_eq!(
            out[0]["params"],
            json!({ "request_id": "tcmbm", "behavior": "deny" })
        );
    }

    #[test]
    fn malformed_permission_requests_are_not_relayed() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        for params in [
            r#"{}"#,
            r#"{"request_id":"abcdl"}"#,
            r#"{"request_id":"ABCDE"}"#,
            r#"{"request_id":"abcdef"}"#,
            r#"{"request_id":5}"#,
        ] {
            let line = format!(
                r#"{{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{params}}}"#
            );
            assert!(server.on_line(line.as_bytes()).is_empty());
        }
        assert!(rx.try_recv().is_err());
        assert!(is_request_id("abcde") && !is_request_id("abcle"));
    }

    #[test]
    fn without_a_hub_reply_says_why() {
        for reason in [NoHub::Headless, NoHub::NoSession, NoHub::NoConfig] {
            let mut server = Server::new(Hub::Off(reason));
            init(&mut server);
            let answer = one(
                &mut server,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"reply","arguments":{"text":"hi"}}}"#,
            );
            assert_eq!(answer["result"]["isError"], true);
            assert_eq!(answer["result"]["content"][0]["text"], reason.text());
            // Permission requests are accepted silently.
            assert!(
                server
                    .on_line(br#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"abcde"}}"#)
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_full_hub_queue_is_a_tool_error() {
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(AgentMsg::Reply { text: "x".into() }).unwrap();
        let mut server = Server::new(Hub::Link(tx));
        init(&mut server);
        let answer = one(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"reply","arguments":{"text":"hi"}}}"#,
        );
        assert_eq!(answer["result"]["isError"], true);
    }

    #[test]
    fn long_replies_are_capped() {
        let (mut server, mut rx) = linked();
        init(&mut server);
        let text = "\u{1}".repeat(MAX_REPLY + 1);
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"reply","arguments":{"text":text}}});
        one(&mut server, &call.to_string());
        let AgentMsg::Reply { text } = rx.try_recv().unwrap() else {
            panic!("reply expected");
        };
        assert!(text.len() <= MAX_REPLY && text.ends_with('\u{2026}'));
        assert!(crate::wire::encode(&AgentMsg::Reply { text }).len() < crate::wire::MAX_LINE);
        // Worst case for a permission request: three escaped fields.
        let field = "\u{1}".repeat(MAX_PERMISSION_FIELD * 2);
        let params = json!({"request_id":"abcde","tool_name":field,"description":field,"input_preview":field});
        let note = json!({"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":params});
        assert!(server.on_line(note.to_string().as_bytes()).is_empty());
        let request = rx.try_recv().unwrap();
        assert!(crate::wire::encode(&request).len() < crate::wire::MAX_LINE);
        assert_eq!(cap("é".repeat(10), 5), "é\u{2026}");
    }

    #[test]
    fn meta_keys() {
        for good in ["a", "A_1", "_", "chat_id"] {
            assert!(is_meta_key(good), "{good}");
        }
        for bad in ["", "a-b", "a.b", "a b", "ä", "a\n"] {
            assert!(!is_meta_key(bad), "{bad:?}");
        }
    }
}

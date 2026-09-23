#!/usr/bin/env python3
# Applies the reviewer-2 fixes (ping, JSON-RPC envelope, version negotiation,
# duplicate permission ids) to reviewer2/ws. Run once, from any directory.
import os

ws = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg')
s = ''


def rep(old, new):
    global s
    assert s.count(old) == 1, old[:120]
    s = s.replace(old, new)


p = os.path.join(ws, 'src', 'channel.rs')
s = open(p, encoding='utf-8').read()
rep('''//! Implemented: `initialize` (channel, permission relay and tools
//! capabilities), `notifications/initialized`, `tools/list`, `tools/call`
//! (`reply`), the incoming `notifications/claude/channel/permission_request`
//! and the outgoing `notifications/claude/channel` and
//! `notifications/claude/channel/permission`. Every other request answers
//! method-not-found; other notifications are ignored.''',
'''//! Implemented: `initialize` (channel, permission relay and tools
//! capabilities), `notifications/initialized`, `ping`, `tools/list`,
//! `tools/call` (`reply`), the incoming
//! `notifications/claude/channel/permission_request` and the outgoing
//! `notifications/claude/channel` and `notifications/claude/channel/permission`.
//! Every other request answers method-not-found; other notifications are
//! ignored. Only JSON-RPC 2.0 (`"jsonrpc": "2.0"`, structured `params`) is
//! served; anything else is an invalid request.''')
rep('''/// Answered when the client names no version; any named one is echoed, since
/// nothing here depends on a protocol revision.
pub const FALLBACK_PROTOCOL: &str = "2025-06-18";
pub const LATEST_PROTOCOL: &str = "2025-11-25";
''', '''/// The MCP revision answered when the client asks for one we do not know
/// (Claude Code 2.1.280 asks for this one).
pub const LATEST_PROTOCOL: &str = "2025-11-25";
/// Revisions echoed back as asked: everything used here (tools, experimental
/// capabilities, instructions) means the same in each of them.
pub const SUPPORTED_PROTOCOLS: [&str; 4] =
    [LATEST_PROTOCOL, "2025-06-18", "2025-03-26", "2024-11-05"];
''')
rep('''        let id = msg.get("id").filter(|id| id.is_string() || id.is_number());
        match (msg.get("method"), msg.get("id")) {
            (Some(Value::String(method)), None) => {
                self.on_notification(method, msg.get("params"));
                self.flush_if_ready(method)
            }
            (Some(Value::String(method)), Some(_)) => match id {
                Some(id) => vec![self.on_request(id, method, msg.get("params"))],
                None => vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")],
            },
            // A response to a request of ours: we send none, so drop it.
            (None, Some(_)) if msg.contains_key("result") || msg.contains_key("error") => {
                Vec::new()
            }
            _ => vec![error(''', '''        // A response to a request of ours: we send none, so drop it.
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
                Some(id) => vec![self.on_request(id, method, params)],
                None => vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")],
            },
            _ => vec![error(''')
rep('''        let Hub::Link(outbox) = &self.hub else {
            return;
        };
        let request = PermissionRequest {''', '''        let Hub::Link(outbox) = &self.hub else {
            return;
        };
        if self.open_permissions.contains(&request_id) {
            debug!("permission request already open; not relayed again");
            return;
        }
        let request = PermissionRequest {''')
rep('''            "initialize" => {
                let version = params
                    .and_then(|params| params.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .filter(|version| !version.is_empty())
                    .unwrap_or(FALLBACK_PROTOCOL);
                result(''', '''            "initialize" => {
                let Some(asked) = params
                    .and_then(|params| params.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .filter(|version| !version.is_empty())
                else {
                    return error(id, INVALID_PARAMS, "initialize needs a protocolVersion string");
                };
                let version = if SUPPORTED_PROTOCOLS.contains(&asked) {
                    asked
                } else {
                    LATEST_PROTOCOL
                };
                result(''')
rep('''            "tools/list" => result(id, json!({ "tools": [reply_tool()] })),''',
    '''            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": [reply_tool()] })),''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)

p = os.path.join(ws, 'tests', 'agent_stdio.rs')
s = open(p, encoding='utf-8').read()
rep('''    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,''', '''    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,''')
rep('''    "",
    r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#,''', '''    "",
    r#"{"id":7,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":8,"method":"tools/list","params":5}"#,
    r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#,''')
rep('''    // Six answers (ids 1-5 and one parse error), no notification.
    assert_eq!(run.stdout.len(), 6, "{}", run.stdout_raw);''', '''    // Nine answers (ids 1-6 and 8, one parse error, one invalid request
    // without an id), no notification.
    assert_eq!(run.stdout.len(), 9, "{}", run.stdout_raw);''')
rep('''    assert_eq!(by_id(&run, 5)["result"]["tools"][0]["name"], "reply");
    assert!(
        run.stdout
            .iter()
            .any(|value| value["id"].is_null() && value["error"]["code"] == -32700)
    );''', '''    assert_eq!(by_id(&run, 5)["result"]["tools"][0]["name"], "reply");
    assert_eq!(by_id(&run, 6)["result"], serde_json::json!({}));
    assert_eq!(by_id(&run, 8)["error"]["code"], -32600);
    for code in [-32700, -32600] {
        assert!(
            run.stdout
                .iter()
                .any(|value| value["id"].is_null() && value["error"]["code"] == code),
            "{}",
            run.stdout_raw
        );
    }''')
rep('''        assert_eq!(run.stdout.len(), 6, "{}", run.stdout_raw);''',
    '''        assert_eq!(run.stdout.len(), 9, "{}", run.stdout_raw);''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('fixed')

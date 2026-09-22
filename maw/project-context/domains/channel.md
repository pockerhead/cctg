# Domain: channel
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- A Channel is an MCP server over stdio, spawned by Claude Code. Protocol: JSON-RPC 2.0, one JSON object per line. Hand-rolled on `serde_json`; do NOT add `rmcp` or any MCP crate.
- Minimal surface to implement: `initialize` (return capabilities and instructions), `notifications/initialized`, `tools/list`, `tools/call`, outgoing `notifications/claude/channel` and `notifications/claude/channel/permission`, incoming `notifications/claude/channel/permission_request`. Every other method answers JSON-RPC method-not-found.
- Registration: `capabilities.experimental["claude/channel"] = {}`, optionally `"claude/channel/permission" = {}` and `tools: {}`.
- Inbound to Claude: `notifications/claude/channel` with `{ content: string, meta: Record<string,string> }`. Meta keys must match `[A-Za-z0-9_]+`, otherwise they are silently dropped. Meta becomes attributes of the `<channel source=... key=...>` tag.
- Outbound from Claude: our tool (e.g. `reply(chat_id, text)`) is called via `tools/call`; the text is not shown in the terminal.
- Permission relay: incoming `{ request_id, tool_name, description, input_preview }`; `request_id` is 5 lowercase letters without the letter l. Reply `{ request_id, behavior: "allow" | "deny" }`. Terminal dialog stays open in parallel, first answer wins. Trust dialogs and MCP consent are never relayed.
- Launch: `claude --dangerously-load-development-channels server:cctg`. Register the server at user scope (`claude mcp add --scope user cctg -- cctg agent`, lands in top-level `mcpServers` of `~/.claude.json`): no per-project consent dialog, works in every folder. Project `.mcp.json` is not used. The `--channels` flag rejects our server (Anthropic allowlist). A session started without the flag gets hooks and a topic but no channel; the hub shows that state.
- Messages reach Claude only while the session is alive; the hub buffers everything addressed to a dead session.
- The channel server does not know its own session id; it reads env `CLAUDE_CODE_SESSION_ID` (inherited from the claude process) and the hub matches it to the hook's `session_id`.
- stdout is reserved for JSON-RPC only. All logging goes to stderr or a file; one stray print on stdout breaks the transport.

## Risk lessons
- 2026-09-22 (TASK-002 QA): `tracing_subscriber::fmt()` writes to stdout by default; `.with_writer(std::io::stderr)` is load-bearing. A stdout-purity test is vacuous unless the code path actually emits a `tracing` event: the test must trigger at least one log line (e.g. `RUST_LOG=trace` plus a real `tracing::info!` in the path) and assert stdout stays empty.

## Pointers
- `CLAUDE.md` (repo root), section "Channels": verified facts, Claude Code 2.1.278.
- https://code.claude.com/docs/en/channels-reference : official reference.
- `anthropics/claude-plugins-official/external_plugins/fakechat` : minimal reference implementation.
- https://github.com/gohyperdev/hdcd-telegram : Rust hand-rolled channel plus permission relay (read for decisions, do not copy).

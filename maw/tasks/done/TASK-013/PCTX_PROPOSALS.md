# PCTX proposals from TASK-013 (planner)

## 2026-09-23 — channel domain: `/clear` keeps the channel server (observed)

Add to `domains/channel.md` Invariants: VERIFIED 2026-09-23 (TASK-013, Claude Code 2.1.280): `/clear` does NOT restart the channel MCP server. The same process keeps env `CLAUDE_CODE_SESSION_ID` of the pre-clear session, Claude Code logs `Channel notifications registered` again, and inbound plus permission relay reach the post-clear session. The agent therefore reports its own claude pid in `Register.claude_pid` and the hub follows it through `pids` (SessionStart source=clear). Evidence: `maw/tasks/in_progress/TASK-013/scratch/clear_evidence.txt`, `probe_log_clear.jsonl`, `debug_CLEAR.log`, `live_hub.jsonl`.
Why: this closes QA O2 of TASK-011; without it the next planner re-probes.

## 2026-09-23 — channel domain: dev channel via `--mcp-config` (observed)

Add to Risk lessons: a server defined only in a temporary `claude --mcp-config <file>` is accepted by `--dangerously-load-development-channels server:<name>` (debug: `Channel notifications registered`). Live probes should use this instead of `claude mcp add --scope user`: nothing leaks into other sessions and no `~/.claude.json` write is needed (Claude Code still adds a `projects[<cwd>]` key for a new folder).

## 2026-09-23 — channel domain: the MCP server's parent is its own claude

Observed twice (probe ppid == spawned claude pid; `cctg agent` registered `claude_pid` equal to the launched claude.exe pid): a stdio MCP server declared with an absolute exe path is a direct child of its claude process. `proctree::current_lineage(None, None, "")` from the agent gives the session's own claude pid.

## 2026-09-23 — channel domain: startup banner can be folded

On 2.1.280 with `--debug-file` the channels banner was folded into "N more notices hidden" on the start screen (TASK-013 CLEAR and LIVE runs). The debug-log line `Channel notifications registered` is the reliable evidence, not the screen.

## 2026-09-23 — channel domain: MCP envelope, ping and version negotiation (plan-reviewer-2)

Replace "Every other method answers method-not-found" with: `ping` answers an empty result `{}` at any time (MCP basic utilities); every other request answers `-32601`, unknown notifications are ignored. Only JSON-RPC 2.0 is served: `jsonrpc` must be exactly `"2.0"` (else `-32600`, `id: null`), `params`, when present, must be an object or array (else `-32600`, the request id echoed). `initialize` needs a non-empty string `protocolVersion` (else `-32602`); a revision from the supported set (2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05) is echoed, anything else gets 2025-11-25. Never echo an unknown revision: the MCP TypeScript client throws "Server's protocol version is not supported" on a revision it does not know. A permission `request_id` that is already open is neither relayed nor queued again.
Why: all four were defects in the TASK-013 planner reference, reproduced by failing tests in `scratch/reviewer2/repro_before_fix.txt`.

> RESOLVED: all five folded into domains/channel.md on 2026-09-23 (with the QA round-2 permission rule replacing the capacity rule).

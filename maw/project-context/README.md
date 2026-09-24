## Orientation
cctg is a Rust bridge between local Claude Code sessions (several devices, several folders) and one private Telegram forum. One cargo workspace, one binary `cctg` with subcommands `hub` (Telegram bot + TCP server + session registry), `agent` (Channel MCP server over stdio, spawned by Claude Code) and `hook` (SessionStart/SessionEnd/Stop/SubagentStart/SubagentStop hooks that POST to hub), plus a pure library crate `transcript` (jsonl parser + brief/full renderers). No Node on any machine.
Architectural law: a Telegram topic is a slot `(device, folder, ordinal)`, not a session. A new session takes the first free slot of its folder; concurrent sessions in one folder get `#2`, `#3`. Sessions succeed each other inside a slot. Subagents and nested `claude -p` runs never get a topic or a slot; they live inside the parent's topic.
The repository's `CLAUDE.md` is the source of truth for platform facts (channel protocol, jsonl format, hooks). Read it before planning; do not re-research what it marks as verified.

## Universal invariants
- Secrets (bot token, hub shared secret) and Telegram user ids never appear in logs, tests, fixtures or commits. Config comes from `.env`; `.cctg/` and `registry.json` are gitignored.
- Surgical changes: touch only what the task names; no speculative abstractions, no "just in case" layers. Every crate must fit in one head.
- Errors are `anyhow::Result` at binary edges and typed where a caller branches on them; no `unwrap()` on external input (network, stdin, jsonl).
- Deserialize only the fields you need with `#[serde(default)]`; never model the whole jsonl or Bot API.
- Language: code, identifiers, commits in English; prose to the user in Russian.
- Commit messages carry no "Generated with" / "Co-Authored-By" trailers.
- Never put a window or a question on the user's screen. Prefer unit/integration tests and `claude -p` (no dialogs; no channel). A live interactive `claude` probe starts in a HIDDEN console (`Popen(..., creationflags=CREATE_NEW_CONSOLE, startupinfo=si)` with `si.dwFlags |= STARTF_USESHOWWINDOW; si.wShowWindow = 0`), answers the workspace-trust and development-channels dialogs itself via `AttachConsole` + `WriteConsoleInputW` (no flag or setting skips them), reads the screen the same way, reuses one fixed probe folder, and kills only its own process tree.
- Every cargo build (also in worktrees and %TEMP% reference workspaces) uses the main tree's target dir: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target` with `CARGO_PROFILE_DEV_DEBUG=0` and `-j 1`; never delete it. Parallel builds serialise on cargo's file lock ("Blocking waiting for file lock" is normal). Per-agent %TEMP% target dirs are no longer used (user rule 2026-09-24: no duplicate build artifacts).

## Domain catalog
- trigger: any file under `crates/transcript/**`, any `.jsonl` fixture, or literal tokens `parentUuid`, `isSidechain`, `tool_result` → {PCTX}/domains/transcript.md
- trigger: any file under `crates/agent/**` or literal tokens `notifications/claude/channel`, `tools/call`, `permission_request`, `--dangerously-load-development-channels` → {PCTX}/domains/channel.md
- trigger: any file under `crates/hub/**` or literal tokens `teloxide`, `create_forum_topic`, `message_thread_id`, `registry.json` → {PCTX}/domains/hub.md
- trigger: literal tokens `SessionStart`, `SessionEnd`, `SubagentStart`, `SubagentStop`, `CLAUDE_CODE_SESSION_ID`, `transcript_path` or any file under `crates/hook/**` → {PCTX}/domains/hooks.md

HARD RULE: before you plan or write any part whose work matches a trigger
above — even if this task was not framed as being in that domain — you MUST
read the mapped module from disk first and treat it as normative. Do not
proceed on that part without it.

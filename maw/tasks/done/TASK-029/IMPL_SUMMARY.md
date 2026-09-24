# IMPL_SUMMARY — TASK-029 (pinned status message + interrupt)

Written by the fixer stage: the implementer stage was an orchestrator patch apply and left no summary. The facts below come from OPEN_DECISIONS.md, metrics.md and `git diff 6c52fc5 0fa7237 -- crates docs` (34 files, +3592/-57).

## How it was implemented
- The PLAN_FINAL reference patch (`scratch/reviewer2`, 34 file hashes) was applied whole on `feature/status-interrupt` after a dead parallel implementer's partial edits were dropped. Hashes matched, fmt and clippy were clean and the workspace was green (614 passed, 1 ignored).
- There is no Ctrl+B / ⏬ code. It was removed until a live check succeeds.

## What is in the code
- `hub/status.rs` (new, pure): `Activity` (turn, running calls, finished ids, interrupt notes, "Esc sent"), `phase`, `render` (emoji line, metrics line, ⏹ / confirm button), `Metrics`, callback parsing.
- `hub/slots.rs`: one status message per slot. It is created for a live session after its separator, pinned once and edited at most once per `STATUS_EVERY` (5 s). A deleted message is sent again. A dead slot shows 🏁. Nested runs and subagents get nothing. ⏹ has a 10 s confirmation, is hidden while a prompt waits, and is sent only to the slot's live agent that announced `console_keys` (`KeyAsk` bound to slot + session + conn).
- The bot's own pin notice is deleted; a person's pin stays. This is in `hub/mod.rs` and `hub/updates.rs`.
- `keys.rs` + agent: `FreeConsole` / `AttachConsole(claude pid)` / `WriteConsoleInputW` Esc, in one bounded worker. `ConsoleKeyWritten` answers the hub.
- `wire.rs`: `Register.console_keys`, `HubMsg::ConsoleKey`, `AgentMsg::ConsoleKeyWritten`, `HookEvent::{StatusLine, ToolStart, ToolEnd}`. `VERSION` is still 1.
- `statusline.rs` + `cctg statusline`: posts the numbers (80 ms budget) and chains the user's `statusLine.command` byte for byte with its exit code. Without a command it prints its own short line.
- `hook.rs`: async `PreToolUse`/`PostToolUse` status hooks (`docs/hook-settings.json`, `docs/poc.md`).
- Tests: `tests/status_e2e.rs`, `tests/statusline_cli.rs` and unit tests. The fixes of the review follow in FIX_SUMMARY.md.

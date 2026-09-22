# Domain: hooks
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- Hooks `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit` receive JSON on stdin: `session_id`, `cwd`, `transcript_path`, `source` (`startup|resume|clear|compact|fork`). This is the only reliable source of session id and transcript path.
- `SessionStart` does NOT fire for subagents. Use `SubagentStart` / `SubagentStop` (fields: `agent_id`, `agent_type`, parent `session_id`; `SubagentStop` carries `last_assistant_message`).
- Children of a session (Bash tool, hooks, MCP servers) inherit env `CLAUDECODE=1`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION=1`.
- Nested `claude -p` runs fire `SessionStart` like a normal session. Nesting detection: hook env has `CLAUDECODE=1` and `CLAUDE_CODE_SESSION_ID` differs from stdin `session_id`, so it is nested and the parent is known. Fallback if the child claude overwrites env: hook writes its `session_id` to `.cctg/<CLAUDE_PID>` and the chain is matched by ppid. Which of the two actually works is an OPEN QUESTION: verify by running code, do not assume.
- Hooks must be fast and fire-and-forget: POST to hub with a short timeout, never block Claude Code, never fail the hook when hub is absent (exit 0, log to stderr).
- Hooks are the `cctg hook <event>` subcommand of the same binary; no scripts, no Node.

## Risk lessons
<!-- dated, one line each -->

## Pointers
- `CLAUDE.md` (repo root), sections "Hooks", "Субагенты", "Вложенные запуски", "Открытые вопросы".

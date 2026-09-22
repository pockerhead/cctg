# Domain: hooks
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- Hooks `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit` receive JSON on stdin: `session_id`, `cwd`, `transcript_path`. `source` (`startup|resume|clear|compact|fork`) is documented for `SessionStart` only; never require it elsewhere. This is the only reliable source of session id and transcript path.
- `SessionStart` does NOT fire for subagents. Use `SubagentStart` / `SubagentStop` (fields: `agent_id`, `agent_type`, parent `session_id`, `transcript_path` of the parent, `agent_transcript_path` of the subagent, `last_assistant_message`).
- Since 2.1.271 a subagent using `SubagentHandback` delivers its report through that tool; `last_assistant_message` is then only closing text. The report is `tool_input.message` in a `PreToolUse`/`PostToolUse` hook matched on `SubagentHandback`.
- `SubagentStop` also fires for Claude Code's internal agents (prompt suggestions, `/btw`): `agent_type` is then empty, or equals the session's `--agent` name. Correlate with a preceding `SubagentStart`/Agent tool call; drop the rest.
- `transcript_path` is written asynchronously and may lag the current turn; use `last_assistant_message` for the current turn's text.
- `SessionEnd` hooks share a 1.5 s budget (raised only by an explicit per-hook `timeout`, max 60 s): the POST must time out well under that.
- Children of a session (Bash tool, hooks, MCP servers) inherit env `CLAUDECODE=1`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION=1`. Only `CLAUDECODE` is documented; the others are observed, not promised. Keep every use behind one detection function with a fallback.
- Nested `claude -p` runs fire `SessionStart` like a normal session. Nesting detection: hook env has `CLAUDECODE=1` and `CLAUDE_CODE_SESSION_ID` differs from stdin `session_id`, so it is nested and the parent is known. Fallback if the child claude overwrites env: hook writes its `session_id` to `.cctg/<CLAUDE_PID>` and the chain is matched by ppid. Which of the two actually works is an OPEN QUESTION: verify by running code, do not assume.
- Hooks must be fast and fire-and-forget: POST to hub with a short timeout, never block Claude Code, never fail the hook when hub is absent (exit 0, log to stderr).
- Hooks are the `cctg hook <event>` subcommand of the same binary; no scripts, no Node.

## Risk lessons
<!-- dated, one line each -->

## Pointers
- `CLAUDE.md` (repo root), sections "Hooks", "Субагенты", "Вложенные запуски", "Открытые вопросы".

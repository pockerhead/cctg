# Domain: transcript
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- Path of a session transcript: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. Encoded cwd = path with `:`, `\`, `/`, spaces and `_` replaced by `-` (`C:\Users\user\dev` becomes `C--Users-user-dev`; `_` verified in TASK-004). Prefer `transcript_path` from the hook over re-deriving the path.
- The transcript file may not exist at all: with `CLAUDE_CODE_CHILD_SESSION=1` inherited, Claude Code turns transcript saving off. Missing file is a normal state for `/brief`/`/full`, not a read error.
- Rendering is an ALLOWLIST: only `type: "user"` and `type: "assistant"` records. Real transcripts also contain `attachment` (a third of all records), `atis-latch`, `queue-operation`, `file-history-delta`, `mode`, `permission-mode`, `file-history-snapshot`, `ai-title`, `last-prompt`, `system`, `summary`; any unknown type is skipped, never an error. `ai-title` is read once by a separate pure function for the topic title. `message.content` is an array of blocks `text | tool_use | tool_result | thinking`.
- Every record has `uuid`, `parentUuid`, `timestamp`, `cwd`, `sessionId`, `gitBranch`, `isSidechain`, `isMeta`. Parse only needed fields with `#[serde(default)]`; unknown records must not fail the parse.
- Subagent transcripts live in `<session-id>/subagents/agent-<agent_id>.jsonl` (plus `.meta.json`), same record format, `isSidechain: true`, `agentId` set. They are NOT duplicated in the parent jsonl. The hub gets this path from `SubagentStop.agent_transcript_path`; constructing it is only a fallback. The library never reads `.meta.json` or any file: hub does IO.
- Transcript files are written asynchronously and may lag the turn that just ended; the text of the current turn comes from the hook field `last_assistant_message`, the jsonl is for history.
- `render_brief`: user prompts, final assistant text, one-line tool calls (`Bash: description`, `Edit: file`); `Agent` calls as `↳ <type> <agent_id>` plus subagent summary. `render_full`: adds tool inputs and truncated results, never `thinking`. Subagents always expand in brief form.
- Output is split for Telegram's 4096-char limit; anything longer goes as a file.
- The crate has no IO/network dependencies: `parse(&str) -> Vec<Turn>`. IO lives in hub.
- Tests are mandatory. Fixtures are anonymized slices of real jsonl from `~/.claude/projects`, stored in `crates/transcript/tests/fixtures/`. Never commit real user ids, tokens or private paths in fixtures.

## Risk lessons
<!-- dated, one line each; folded via maw-context --review -->

## Pointers
- `CLAUDE.md` (repo root), sections "Транскрипты сессий" and "Субагенты": verified format facts.
- `~/.claude/projects/C--Users-user-dev-cctg/*.jsonl`: live examples on this machine for fixture extraction.

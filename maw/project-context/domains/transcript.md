# Domain: transcript
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- Path of a session transcript: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. Encoded cwd = path with `:`, `\`, `/`, spaces replaced by `-` (`C:\Users\user\dev` becomes `C--Users-user-dev`).
- Only `type: "user"` and `type: "assistant"` records are rendered. `message.content` is an array of blocks `text | tool_use | tool_result | thinking`. Ignore `mode`, `permission-mode`, `file-history-snapshot`, `ai-title`, `last-prompt`, `system`, `summary` (but `ai-title` is read once for the topic title).
- Every record has `uuid`, `parentUuid`, `timestamp`, `cwd`, `sessionId`, `gitBranch`, `isSidechain`, `isMeta`. Parse only needed fields with `#[serde(default)]`; unknown records must not fail the parse.
- Subagent transcripts live in `<session-id>/subagents/agent-<agent_id>.jsonl` (plus `.meta.json`), same record format, `isSidechain: true`, `agentId` set. They are NOT duplicated in the parent jsonl.
- `render_brief`: user prompts, final assistant text, one-line tool calls (`Bash: description`, `Edit: file`); `Agent` calls as `↳ <type> <agent_id>` plus subagent summary. `render_full`: adds tool inputs and truncated results, never `thinking`. Subagents always expand in brief form.
- Output is split for Telegram's 4096-char limit; anything longer goes as a file.
- The crate has no IO/network dependencies: `parse(&str) -> Vec<Turn>`. IO lives in hub.
- Tests are mandatory. Fixtures are anonymized slices of real jsonl from `~/.claude/projects`, stored in `crates/transcript/tests/fixtures/`. Never commit real user ids, tokens or private paths in fixtures.

## Risk lessons
<!-- dated, one line each; folded via maw-context --review -->

## Pointers
- `CLAUDE.md` (repo root), sections "Транскрипты сессий" and "Субагенты": verified format facts.
- `~/.claude/projects/C--Users-user-dev-cctg/*.jsonl`: live examples on this machine for fixture extraction.

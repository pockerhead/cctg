# PCTX proposals — TASK-009

## 2026-09-23 (planner): hub, what TASK-009 adds to the "Implemented" line

Proposed addition to `domains/hub.md` Invariants after the TASK-008 line, once TASK-009 is merged:
"TASK-009: `getUpdates` offset persisted in `<CCTG_STATE_DIR|.cctg>/offset` (temp file + fsync + rename), saved before a batch is handled (at most once). `/brief [n] [prefix]` and `/full [n] [prefix]` run on one sequential command worker; `n` = last n prompts via `transcript::last_prompts`. Transcript choice goes through `hub::sessions::TranscriptLocator::locate(thread_id, prefix)`; the TASK-009 impl `ProjectsDir` scans `<CCTG_PROJECTS_DIR|~/.claude/projects>/<project>/<uuid>.jsonl` (direct children only, subagents never). TASK-011 replaces the impl, not the trait."

## 2026-09-23 (planner): hub risk lesson, project directory names are private paths

The encoded-cwd directory name (`C--Users-<name>-dev-app`) is a private path in disguise. It may be shown to the allowlisted user in Telegram, but never logged. `tests/command_logs.rs` guards this with a marker in the project name.

## 2026-09-23 (plan-reviewer-2): hub risk lesson, getUpdates offset after long pauses

Proposed risk lesson for `domains/hub.md`: "Bot API: after a week without updates the next `update_id` is random (can be below the last offset) and updates live at most 24 h. The next offset is `max(update_id in batch) + 1`, never `max(old offset, ...)`, and a persisted offset older than 24 h is ignored on load." Source: https://core.telegram.org/bots/api#update, go-telegram-bot-api issue #156 (endless re-delivery after the reset).

> RESOLVED: folded into domains/hub.md on 2026-09-23.

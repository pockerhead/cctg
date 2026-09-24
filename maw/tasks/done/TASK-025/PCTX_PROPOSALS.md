# PCTX proposals (TASK-025)

## 2026-09-24 (implementer)

- channel domain, TASK-022 line ("`reply` is only for extra messages during the work") and TASK-013 line ("The instructions name the tool `mcp__<server>__reply` and note it may be deferred (ToolSearch)") are outdated after TASK-025: `INSTRUCTIONS` and the `reply` description now say everything the session writes (text, visible thinking, tool lines, final answer) reaches the topic automatically and `reply` is not needed (kept for compatibility); the ToolSearch hint is gone. The `target_agent` -> SendMessage rule and "never ask for permissions through `reply`" stay.
- hub domain, TASK-016 line: the stream now also carries `💭 <thinking>` messages (non-empty `thinking` blocks cut to `transcript::THINKING_LIMIT` = 1000 graphemes + `…`, own message, `merge: false`, flushes finished calls before it like a note) and never shows a call line for `mcp__cctg__reply`.
- transcript domain: `StreamEvent::Thinking(String)` exists (read by `stream_events` from the raw record; `parse`/`Block` still have no thinking and `/brief`/`/full` never show it). Signature-only (empty) `thinking` is real and common; no `redacted_thinking` record was found in the local transcripts (shape taken from the API: `{"type":"redacted_thinking","data":...}`).

> RESOLVED: deferred to the post-MVP condense of domains/hub.md and channel.md (2026-09-24); the TASK-022/TASK-013 channel lines are stale since TASK-025.

# Open decisions — TASK-025 (orchestrator)

- 2026-09-24: review minor 1: thinking now uses `merge: true` (its own message normally, joins neighbouring lines of the topic only when more messages wait than tokens), so it no longer halves merging under the 20/min limit. Unit test updated; workspace green.
- 2026-09-24: review minor 2 (sessions with a channel but no stream would lose progress messages now that INSTRUCTIONS say reply is not needed): accepted; the agent reads the transcript on its own device, so only a session with transcript writing disabled is affected. Recorded as a known limit.
- 2026-09-24: no separate QA stage: the user is testing live right now; the orchestrator ran fmt, clippy on cctg and the full workspace (lib 408, stream_e2e 12 incl. the new thinking/reply e2e) after the fix. Live check by the user follows the merge.

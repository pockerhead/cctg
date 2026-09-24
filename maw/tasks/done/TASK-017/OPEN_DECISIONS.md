# Open decisions — TASK-017 (orchestrator, full autonomy)

- 2026-09-24: premise challenge (codex, interrupted by host process-start failure) raised: a slot can come alive through ordinary slot reuse, not only Resume. Accepted: the topic is the slot, so buffered messages go to whichever live top-level session becomes the slot's current session next (same session resumed, new session, /clear). TASK_FINAL acceptance line 3 amended. The buffer is per slot, not per session.
- 2026-09-24: planner Q2 (live session without an agent): buffered with one QUEUED_NOTICE per period instead of the old "not delivered" notice; nothing the user writes is dropped. Approved.
- 2026-09-24: planner Q1 (exactly-once across a hub crash needs agent-side dedup by message_id): not in this task; at-least-once documented as residual.
- 2026-09-24: planner Q3: TASK-019 (post-MVP, not authorized) reads `buffer.resume_asked` and the session from the note.
- 2026-09-24 (after plan-reviewer-1): findings 1-9 go to plan-reviewer-2; fix only what reproduces, smallest change. At-least-once across a hub crash stays the accepted residual (no wire change here).
- 2026-09-24: reviewer-2 rejections accepted: no Resume button on every SessionEnd until TASK-019 can act on it (noise); stale Resume presses are harmless (same wish, UUID ids). Unidentified hub:: flake (3/10 runs right after a full workspace run, 60 s WAIT): QA must capture the test name.
- 2026-09-24: orchestrator kept the ready-to-paste `claude --resume <full id>` in the Resume text (the fixer had replaced it with "choose this session" and a short id; the full id is not a secret and the command is the useful part).
- 2026-09-24: QA SHIP. Low residuals: a full link queue leaves the rest of the buffer until the next actor event (<=60 s); a SessionEnd lost while the hub was down leaves the slot NoChannel without a Resume button (TASK-011 registry behaviour).

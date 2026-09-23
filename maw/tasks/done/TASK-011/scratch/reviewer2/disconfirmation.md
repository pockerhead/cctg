# Reviewer 2 disconfirmation target

Most concrete input that would make PLAN_V2 wrong:

Known top-level session A (slot #1, pid 10) receives a second SessionStart for the
same session id with `claude_pid = 20, parent_claude_pid = 10` (a nested
`claude -p --resume A` from inside A, or a stale CLAUDE_PID on the device).
`pids["box/10"] == A`, so the parent resolves to the session itself.

PLAN_V2 says only "stale-self -> NestedUnknownParent, no slot". Implemented
literally inside the planner's `session_started` (`entry.kind = kind; entry.slot = slot`),
A becomes `Nested { parent: None }` with `slot: None` while slot #1 still has
`current_session = A`. The next plain SessionStart of A (`parent_claude_pid = None`)
hits `Some(entry) if parent_pid.is_none() => entry.kind.clone()` and keeps the nested
kind forever: A never returns to its own slot, `/brief` routing and the pid map drift.
Zero topics are created, so no PLAN_V2 test would notice.

Status: HELD against a literal reading of PLAN_V2 (code path verified in
planner ws `registry.rs` lines 507-512 and 540-541). PLAN_FINAL fixes it: a
self-resolving parent classifies the start as NestedUnknownParent and returns
`Parent(None)` WITHOUT rewriting the existing record (no slot, kind, pid or
ended change). Test: `registry::a_parent_pid_that_is_the_session_itself_is_nested_unknown_parent`.

Second target (PLAN_V2 defect 4, one-in-flight): Separator and Edit of one slot
issued in the same pass, Edit answered TOPIC_ID_INVALID, then the Separator's
TOPIC_ID_INVALID arrives while the replacement Create is in flight -> second Create.
Status: reproduced against the planner ws with
`slots::a_gone_topic_during_a_session_change_is_replaced_once` (see repro.out.txt).

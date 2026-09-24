# PCTX proposals (TASK-039)

## 2026-09-24: hooks domain, new invariant
`SessionStart`/`SessionEnd` hook posts carry `HookPost.live_claude_pids` (optional, top-level, skipped when `None`): every `claude(.exe)` and `node(.exe)` pid of the device, at most 1024, else no list. Never kept in the spool (`spool::save` drops it). Why: the hub ends dead sessions only from fresh same-host data; a stale or partial list would end live sessions.

## 2026-09-24: hub domain, TASK-039 line
Before a `SessionStart`/`SessionEnd` with a list is applied, the hub ends every non-ended session of that host (top-level and nested) whose `claude_pid` is not listed, exactly as a SessionEnd would (`Followup.ended_sessions` + `reaped`). Never: the posting session, other hosts, sessions without a pid, starts younger than 5 s (`REAP_GRACE`, transient), and nothing when the list lacks the posting event's own `claude_pid`. No agent-disconnect timer; the hub does not reap at start. Why: new sessions got `#2` next to topics of killed sessions (VOIDRUN #2, Git #2).

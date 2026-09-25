# PCTX proposals from TASK-047

## 2026-09-25 (implementer) — channel domain, risk lesson

Claude Code's agent view (a subagent opened from the agent list) changes the
screen shape that `keys::input_box` relies on: in the live case the typed
`/exit` was not found between two rule lines and the notice was
"Обновить клиент не получилось". The view shows `>\u{a0}Message @<agent>…` as
the input line and, under the status lines, an agent list (`( ) main`,
`●   <agent>  <activity> 35m 15s · ↓ 337.7k tokens`). `●` also starts every
tool call line of the conversation, so a detector must anchor on the list's
`main` row. Proposed lesson: anything typed into the claude console
(`/exit`, TASK-043 commands) first checks `keys::agents_block`; a new screen
shape found live goes into its tests as a fixture.

Why: the next task that types into the console (or reads the screen) would
otherwise rediscover this from a failed live run.

## 2026-09-25 (implementer) — hub domain, invariant

A client restart that cut off work (Esc written while the update press
waited, or a turn still running at the `restarting` answer) is marked in
`registry.sessions[..].restart_interrupted` (persisted) and answered by one
channel message to the session's next bound agent; a leaving agent bound
back (restart did not happen) clears it.

Why: the flag outlives the agent link and the update press; later work on
updates or resume must keep this pairing.

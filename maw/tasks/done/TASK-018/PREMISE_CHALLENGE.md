# PREMISE_CHALLENGE — TASK-018

## 1. Counter-example tested

The fifth top-level session in folder A starts while the hub is down. Its
`SessionStart` hook is a one-shot POST (fire-and-forget, exit 0 when the hub is
absent), and the hub "never adopts a session without a SessionStart". If nothing
in the repo replays or re-derives that SessionStart after the hub returns (hook
spool, agent `Register` carrying enough to adopt, reconciliation from
transcripts), then the fifth session stays unknown to the hub forever: it cannot
"after the hub returns occupy the freed slot, write one separator and create no
topic". The acceptance criterion would then be unmeetable by the product as it
stands, or meetable only vacuously ("no topic created" because the session is
never seen at all), i.e. the soak would be testing a recovery path that does not
exist.

## 2. Primary-source investigation

- `crates/cctg/src/hook.rs:71-74`: the hook POSTs once; on failure it only logs
  `warn!(..., "hook event not delivered")` and returns. No spool, no retry after
  the process exits. A SessionStart fired while the hub is down is gone.
- `crates/cctg/src/hub/slots.rs:503-518`: an agent that registers for a session
  the registry does not know is parked in `self.pending` with the log
  "agent of an unknown session waits for its SessionStart". `Register` does not
  create a session.
- `crates/cctg/src/hub/slots.rs:660-670`: the only exit from `pending` is
  `on_hook` when `registry.sessions` already contains the session, i.e. after a
  hook that creates the entry.
- `crates/cctg/src/hub/registry.rs:939-947` (`agent_connected`): binds only an
  existing TopLevel entry, returns false otherwise.
- `crates/cctg/src/hub/registry.rs:2703-2763`: tests pin this behaviour: "An agent
  of an unknown session is not adopted" (`!registry.agent_connected(B, 2)`,
  `!registry.sessions.contains_key(B)`), and `UserPromptSubmit`/`Stop` of an
  unknown session create nothing (`Followup::default()`, no slots, no topic work).
- Grep over `maw/tasks/*/task.md` for any hub-down / adoption work: nothing in
  pending (TASK-019, TASK-024) or in the prerequisites TASK-014..017 adds it.
- Ran: `CARGO_PROFILE_DEV_DEBUG=0 cargo test -j 1 -p cctg --lib -- unknown_session a_hook_only_session_is_bound`
  Output:
  ```
  test hub::registry::tests::a_hook_only_session_is_bound_by_its_agent_later ... ok
  test hub::registry::tests::prompt_hooks_of_an_unknown_session_create_nothing ... ok
  test hub::slots::tests::unknown_sessions_make_no_topics_until_their_session_start ... ok
  test result: ok. 3 passed; 0 failed
  ```

## 3. Did it hold

Yes, the counter-example holds. A top-level session whose SessionStart was sent
while the hub was down is never learned by the hub after it returns: its agent
waits in `pending` forever, its prompts and Stop hooks are ignored, and it gets
no slot, no separator, no routing. It only becomes known if the same session
later emits another SessionStart (`/clear`, `--resume`, compact), which is not
part of the scenario. So acceptance criterion 2 ("the fifth session, started
with the hub down, after its return takes the freed slot, writes one separator
and creates no topic") cannot be met by the code the task treats as finished
(TASK-014..017). Its "creates no topic" half would pass vacuously, which is the
dangerous part: a soak could report green while the session is simply invisible
and inbound from the `[host] A` topic keeps going to the buffer of the dead
session. The rest of the premise (three topics for 2+1 top-level sessions, nested
run without a topic, scheduler/429/permission-first checks) was not contradicted
by anything I found; this audit did not try to break it further.

## 4. Verdict

PREMISE SUSPECT — `crates/cctg/src/hook.rs:72-74` sends SessionStart once and drops it when the hub is absent; `crates/cctg/src/hub/slots.rs:503-518` and `:666-670` park an agent of an unknown session until a SessionStart that never comes; `registry.rs:2716-2719` and the passing test run (`a_hook_only_session_is_bound_by_its_agent_later`, `prompt_hooks_of_an_unknown_session_create_nothing`, `unknown_sessions_make_no_topics_until_their_session_start`: 3 passed) confirm the hub never adopts it ; smallest implied reframing: the "fifth session started with the hub down" scenario is not a soak check of existing behaviour but a missing recovery feature (hub learning a session whose SessionStart it missed), so it must either be split out as its own implementation task or the criterion rewritten to the current contract (hub down at SessionStart means the session stays unknown until its next SessionStart).

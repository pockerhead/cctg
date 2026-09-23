# QA REPORT — TASK-021: hub routes topic messages to the session agent and replies back

## 1. Environment

- Direct (no docker-compose, no dev server). Code under test: branch `feature/hub-message-routing` at `c558bdd`, working directory `C:/Users/user/dev/cctg` (clean tree).
- One `CARGO_TARGET_DIR=%TEMP%/cctg-qa021-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time. Deleted after the run, together with the mutation copy `%TEMP%/qa021-mut` and the `%TEMP%/qa021-*` state dirs.
- No real Telegram API, no `.env`/`device.env` read, no live Claude Code session (the orchestrator runs `docs/poc.md` after merge).
- New scripted end-to-end driver: `maw/tasks/in_progress/TASK-021/scratch/qa/e2e/` (own crate, path dependency on `crates/cctg` and `crates/transcript`). Real `Slots::run` actor, real `Scheduler` with a fast `BucketConfig` (capacity 10000, refill 1 ms, `min_gap` 0), fake `Transport` (topic ids 100, 101, ...; optional stalled sends), fake agent links (`mpsc` receivers). Updates are real Bot API JSON going through `updates::route_batch`, then split like the private `hub::route_inbound` (`commands::is_command` to a command list, the rest as `Control::Message`). Paused current-thread runtime, so the 60 s notice window is checked exactly with the default `Options::notice_every`. A global `tracing` subscriber (`.without_time()`, TRACE) captures every log line.

Reproduce:

```
export CARGO_TARGET_DIR="$TEMP/cctg-qa021-target" CARGO_PROFILE_DEV_DEBUG=0
cargo fmt --all -- --check
cargo clippy -j 1 --workspace --all-targets -- -D warnings
cargo test -j 1 --workspace
cd maw/tasks/in_progress/TASK-021/scratch/qa/e2e && cargo run --offline -j 1
```

## 2. Test results

### Existing suite

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | clean |
| `cargo test -j 1 --workspace`, 4 runs | 4/4 exit 0; cctg lib 237 passed, 1 ignored; every integration binary (incl. `message_logs`, `overflow_logs`, `command_logs`) and doc-test ok |
| `cargo test -p cctg --lib hub::`, 15 runs | 15/15 ok (155 passed, 1 ignored) |
| `--test message_logs --test overflow_logs --test command_logs`, 10 runs | 10/10 ok |

No flakes.

### New tests (scratch/qa/e2e, outputs `scratch/qa/e2e_run1..6.txt`)

26 checks, 6 runs, 0 failures. Sequence:

1. Two live sessions A (pid 10, topic 100) and B (pid 11, topic 101), one agent each. Ten real updates in one batch: text in 100 carrying the implicit topic-root `reply_to_message`; text in 101 with an explicit reply to 55; General; topic 999 (not a slot); a non-allowlisted sender; `forum_topic_created`; `forum_topic_edited`; `/brief 2`; `/QAMARK_SLASH do it`; `  /tmp/QAMARK_PATH.log fails`. Result: A got exactly one Inbound with meta `{chat_id, message_id, thread_id}` (root reply filtered); B got exactly one with `reply_to_message_id=55`; every meta key matches `[A-Za-z0-9_]`; the three slash texts went to the command side; no agent got anything else; no notice was sent.
2. Reply of 4 paragraphs of 3000 chars (4 chunks) plus "short" from B: 5 sends to topic 101 in exact order, the chunks concatenate back to the reply. A 5-chunk reply from A: exactly one `SendDocument` to topic 100 with the whole text and no message chunks.
3. B ends (agent still linked). 10 texts to 101: exactly 1 offline notice, B's agent gets nothing. 3 photos: 1 text-only notice. +57 s: no new notice. +61 s total: exactly one more. A late Reply from B's agent: dropped.
4. `/clear` on A (SessionEnd reason=clear, SessionStart C source=clear, same pid): separator in topic 100, no new topic, the next topic message reaches the same connection as C, C's reply lands in topic 100.
5. That connection drops and re-registers with the stale env id A and pid 10: it is bound to C, gets the next inbound, can reply; a reply from the gone conn is dropped.
6. B resumed in its old slot (new agent) gets inbound. 20 interleaved replies from two slots keep per-topic FIFO.
7. Stalled Telegram: every send hangs; 3000 replies flood the actor; a topic message sent afterwards still reaches the agent.
8. Logs: 347 captured lines. The fixed routing lines are present ("message forwarded to the session agent", "agent reply queued", "message for a session that is not on line; not delivered", "notice sent to this slot recently; not repeated", one overflow warning). No `QAMARK` text, no user id (allowed or stranger), no cwd/folder.

Mutation check of my driver (on a copy of the crates in `%TEMP%`, repo untouched):
- mA: `live_reply_slot` without the live and `current_session` checks. My driver fails "late reply of an ended session dropped". Killed.
- mB: the notice cooldown removed. 4 checks fail (10 notices instead of 1, etc.). Killed.

Probe (reported, not pass/fail): two connections of the same claude pid, both bound to A (the newer one current), then `/clear`. `follow_pid` moves both in `HashMap` order, and the last one moved wins `C.agent`. In 20 trials the stale connection got the next inbound 9 times (`e2e_run6.txt`).

## 3. Acceptance criteria

| # | Criterion | Test performed | Result |
|---|---|---|---|
| 1 | Allowlisted text in a slot topic reaches the agent of the slot's current session as one `Inbound` with correct meta (keys `[A-Za-z0-9_]`); General and foreign topics reach no agent | e2e step 1 (real update JSON, two slots, exact meta equality, key check, General/999/stranger); unit `a_topic_message_reaches_only_the_agent_of_its_slot`, `only_an_explicit_reply_is_a_reply` | PASS |
| 2 | Agent `Reply` goes to its session's slot topic through the scheduler; a long reply is split by `split_for_telegram` and keeps chunk order | e2e step 2 (4 chunks in order + concat check, >4 chunks -> one document, nothing to the other topic), step 6 (interleaved FIFO); code: `on_reply` -> `send_messages` -> dispatch -> `Scheduler` | PASS |
| 3 | A message to a topic with no live session or no agent gives one short notice and no errors; the session bound after `/clear` by pid gets inbound in its slot | e2e step 3 (1 notice per 60 s, text-only notice, late reply dropped), steps 4-5 (`/clear` + stale-id re-register); unit tests `a_burst_to_a_dead_slot_gets_one_notice_a_minute`, `after_clear_...` | PASS (see Bug 2 for a rare race) |
| 4 | Commands and `forum_topic_*` are not forwarded as inbound; a non-allowlisted sender never reaches an agent | e2e step 1; unit `messages_go_to_the_slot_actor_and_commands_do_not` | PASS (see Bug 1: every text starting with `/` counts as a command) |
| 5 | The ingress path and the slots actor never wait for Telegram; with a stalled Telegram inbound keeps reaching the agent | e2e step 7 (3000 replies behind a hung send, inbound still arrives); unit `a_stalled_telegram_never_stalls_inbound`, `the_backlog_of_messages_for_telegram_is_capped`; `overflow_logs` | PASS |
| 6 | No message text, user id or secret in logs | e2e step 8 (full TRACE capture); `message_logs`, `command_logs` (marker now inside the first slash token); review of every new `tracing` call in the diff (fields are ordinal, short session id, conn number, parts count, `%error` of `ApiError`) | PASS |
| 7 | Existing tests pass | 4 workspace runs, fmt, clippy | PASS |

Fixer claims re-checked against code:
- Reply routed only for the live top-level current session of its slot: `Slots::live_reply_slot` (slots.rs) checks conn exists, `is_live_top_level`, `SessionEntry.agent == Some(conn)`, `slot.current_session == session`. Confirmed by e2e steps 3 and 5 and by mutation mA. After `/clear`, `follow_pid` rewrites `Conn.session` before any reply, so valid replies are not lost (e2e step 4).
- Unknown slash commands never logged: `commands::handle` now logs the fixed text "unknown slash command" with no field. Confirmed by code and `command_logs`.
- `docs/poc.md`: the false CLI-reference claim is gone. Commands, the `hub started, polling` log line, `~/.cctg/device.env`, `docs/hook-settings.json` hook names (incl. `PostToolUse` + `SubagentHandback`) and the topic title format match the code. `/brief` without a prefix takes the path from the hook (`SlotLocator`), so it works with `CLAUDE_CONFIG_DIR`, as the doc says.

## 4. Bugs found

### Bug 1 — Minor (UX, by design in the plan): any text starting with `/` is dropped without a word

- Repro: in a slot topic with a live session, send `/tmp/app.log падает, посмотри` or a Claude Code command such as `/compact`.
- Expected: either forwarded to Claude (not a hub command) or a short answer that it was not delivered, like the offline case.
- Actual: `commands::is_command` is `text.trim_start().starts_with('/')`, so the text goes to the command worker, `parse` returns `NotOurs`, only a debug line is written. Claude gets nothing and the user gets no answer (e2e step 1, `commands` list).
- The plan lists "`/anything` still goes to the command worker" as a known behaviour, so this does not block the task. It is still a silent loss, and a message that starts with a Unix path is a normal thing to send. Suggestion for a follow-up: forward `NotOurs` texts to the slot actor, or answer them with a short notice.

### Bug 2 — Minor (rare race, code from TASK-011 that routing now depends on): after `/clear` the new session can bind to a stale duplicate connection

- Repro (e2e `duplicate_clear_probe`): session A, conn 1 and then conn 2 register for A with the same pid (conn 2 is current, conn 1 has not disconnected yet), then `/clear` (A ends, C starts, same pid). Send a topic message.
- Expected: C is bound to the newest connection (conn 2).
- Actual: `follow_pid` goes over `self.conns` (a `HashMap`) and calls `agent_connected` for every conn of the pid, so the last one in hash order wins. The stale conn got the inbound in 9 of 20 trials. The hub logs "message forwarded", and the message goes into a link that is about to close.
- Impact: small. It needs an agent reconnect whose old socket the hub has not seen close yet, at the same moment as `/clear`. Suggested fix: in `follow_pid` bind only the highest conn id (the newest registration), or keep the session's current `agent` when it is among the movers.

### Observations (no action for this task)

- Replies over the 256 backlog are dropped whole, and the `reply` tool has already told Claude "Sent". This is in the plan's limitations. Seen in e2e step 7: one overflow warning, then silent drops.
- A session started without `--dangerously-load-development-channels` still has an agent (user-scope server). The hub forwards to it and Claude Code drops the message without a word. The hub cannot tell this case today; that belongs to the "no channel" state work, not this task.
- `/brief <prefix>` (explicit prefix) still searches `~/.claude/projects`, so with the PoC's `CLAUDE_CONFIG_DIR` only the prefix-less form works. The doc only promises the prefix-less form, so it is accurate.

## 5. Verdict

**SHIP.**

Every acceptance criterion passes in the unit tests, in the new end-to-end driver (26 checks, 6 runs) and in 4 full workspace runs with no flakes. Both major review findings are fixed: my own mutation confirms the late-reply gate, and the unknown-command log carries no text. No text, user id or path reached the logs. The two bugs are minor. One is a design choice the plan wrote down; the other is a rare race in pre-existing `follow_pid` code. Both are worth follow-up tasks, and neither breaks the PoC path in `docs/poc.md`.

Not executed: the live PoC with a real bot and a real Claude Code session (out of scope for this stage, and it needs the real token).

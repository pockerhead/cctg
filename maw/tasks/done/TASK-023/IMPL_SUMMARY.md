# IMPL_SUMMARY — TASK-023 (small-fix)

Pre-flight: every name from the task and QA_REPORT_2 exists with the assumed shape (`Scheduler::break_stream`/`broken`/`Op::Stream.restart`, `Slots::on_chunk` → `Action::Release` → `release` → `send_text` (`Op::Send`), `Live::rewind`, `Options::hold_answer`/`stream_retry`, test `a_new_agent_process_goes_on_from_the_stream_position`). No PLAN_BLOCKED.

## 1. What was implemented

| File | +/- | Change |
|---|---|---|
| `crates/cctg/src/hub/stream.rs` | +95 / -9 | `Entry::Message` carries `answer: Option<Held>`; `Live::sent_answer(held)`; `Live::rewind(offset, calls, at, hold)` puts answers that are not in the topic (refused, or dropped unsent behind a refused line) back into `held`, before the ones still held, with `until >= at + hold`; module doc; unit test `a_rewind_holds_again_the_answers_not_in_the_topic_before_the_held_ones` |
| `crates/cctg/src/hub/slots.rs` | +225 / -45 | new `answer_ops(live, held)`: a streamed answer becomes `Op::Stream { merge: false, restart: take(live.restart) }` messages (split_for_telegram chunks, the last one tracked with the `Held`); `Err(held)` when it prefers a file. `release()` uses it whenever the session has a stream target, else the old `send_text`. `on_chunk`: the turn end pops the held answer and queues its messages at that step (so entries, `restart` and hand-off order stay the line order; the `releases` counter is gone). `on_turn_answer`: the claim path (turn end read before the Stop) goes through `release()` too. `on_stream_done` passes `hold_answer` to `rewind`. New `stream_answer()` hands the ops to dispatch and logs `turn answer queued`. Tests: `an_answer_behind_a_refused_line_is_held_again_and_follows_the_re_read_lines` (sync harness, claim path + refusal + rewind + re-read); `a_new_agent_process_goes_on_from_the_stream_position` rewritten deterministic (see below) with a `gated_reader`/`ReadGate` rig helper |
| `crates/cctg/tests/stream_e2e.rs` | +117 / -5 | fake refuses by substring once (`refuse_once`); new `e2e_answer_after_a_refused_line_still_follows_its_lines` = QA2 `refusal_run` ported: real `cctg agent` + real `serve_agents` over TCP, bucket capacity 2 / 300 ms, refusals on `c05` and `c11`, Stop after the first third of the file; asserts first appearances `> gamma warm, > gamma run, c00..c15, GAMMA DONE`, the answer exactly once, 2 refusals, answer within `hold_answer + stream_retry + 3 s` of the Stop |

How it works: the answer no longer bypasses the topic's `broken` gate. It is a stream message after the lines of its turn, so a refused line before it makes the scheduler drop it unsent like any later line; `on_stream_done` sees it unaccepted, the stream rewinds, `rewind` holds the answer again, and the re-read turn end sends it after the re-sent lines. No loss: an answer entry is only forgotten once Telegram accepted it (or a 4xx skipped it, as for lines). No-stream sessions (TASK-022) are untouched: `release`/`on_turn_answer` fall back to `send_text` without a stream target.

Scenario "rotation during a refusal" (AC3): confirmed by a probe, not fixed. `rotation_probe.rs.txt` (run in the slots unit harness, then removed from the tree; output `rotation_probe.out.txt`): line refused → rewind → `SessionEnd` → `pump` drops the ended session's `Live` (nothing in flight) → committed offset stays 0 < 10, the refused line is never sent again. Cause: `pump_streams` stops streaming a session the moment it is no longer `current_slot` and forgets its `Live` once `unanswered() == 0`, which is always true right after a rewind. Only held/re-held answers survive (they are released via `send_text`). Fixing needs draining the ended session's stream up to its file end before the next session's separator (the separator is `pending_separator`, and reads need an agent bound to that session, which `/clear` rebinds), which is a design change beyond small-fix. Proposed follow-up task: "hub: drain an ended session's stream (refused/unread lines) before the next session's separator in the slot".

## 2. Deviations / not implemented

- Rotation loss not fixed (reason above); separate task proposed.
- Behaviour changes to note for review:
  - A streamed session's answer is now `Op::Stream`, so a permission prompt of the same topic may overtake a queued answer (stream lines already yield to prompts).
  - Streamed answers are not refused by `MAX_QUEUED_MESSAGES` (they count in `queued_messages`, like stream lines, but are not dropped at the cap); the stream bounds itself per session.
  - An answer that prefers a file (> 4 parts) still goes as a document outside the stream (not gated).
  - A multi-part answer with a refused later part is sent again whole (at-least-once, duplicate first part possible).
- `cargo fmt` collapsed `Entry::Barrier { to, calls }` to one line (forced by fmt, no semantic change).

## 3. Test results (scratch/implementer/)

- Before the fix: `cargo test -p cctg --test stream_e2e e2e_answer_after` FAILED, `GAMMA DONE` before `c00..c15` (`e2e_answer.before.out.txt`), same trace as QA2.
- After: passed 3/3 with trace, answer last, sent once (`e2e_answer.after.trace.out.txt`).
- `cargo fmt --all -- --check` OK (`fmt.out.txt`); `cargo clippy -j 1 --workspace --all-targets -- -D warnings` rc=0 (`clippy.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast` rc=0: cctg lib 370 passed / 1 ignored, stream_e2e 7, all other binaries green (`workspace_test.out.txt`).
- Mutations (`mutate.py`, restores files byte for byte): R11 (`conn != Some(asked)` removed) KILLED 3/3 by the rewritten test (`mutation_r11.out.txt`, fails at the `< 5 s` bound after the 10 s timeout). Fix mutations (`mutations_fix.out.txt`): release always plain (A1) KILLED by the unit test; no re-hold in `rewind` (A2) KILLED by both unit tests and by the e2e (A4, the answer is lost, timeout); `answer_ops` always plain (A6, = old behaviour) KILLED by the e2e. A3/A5 are equivalent mutants (the `Release` action goes through `release()`, which is gated as well); A1 is not reachable from the e2e scenario (its answer is released in `on_chunk`).

`a_new_agent_process_goes_on_from_the_stream_position` determinism: the first agent is a `gated_reader`; after `> one` the test sets `gate.stopped`, waits until the reader has parked a read without answering (so a read to conn 1 is surely in flight), then disconnects conn 1. The test waits on conditions, not on sleeps.

## 4. Manual verification

1. `set CARGO_TARGET_DIR=%TEMP%\cctg-023-target`, `CARGO_PROFILE_DEV_DEBUG=0`.
2. `cargo test -j 1 -p cctg --test stream_e2e e2e_answer_after -- --nocapture`: the trace shows the refused merges, the `restart` lines, and `["GAMMA DONE"]` as the last accepted stream message.
3. `cargo test -j 1 -p cctg --lib an_answer_behind a_rewind_holds a_new_agent_process`.
4. Live (outside this task, needs Telegram): an answer in a topic after a Telegram 5xx during a turn must show after all tool lines of that turn.
5. Delete `%TEMP%\cctg-023-target` afterwards.

## 5. Known limitations (added by the fixer, review I3-I6)

- I3: after a rewind a held-again answer waits for its own turn end at most until `stream_retry + hold_answer`. If the re-read comes later (queue at `STREAM_QUEUE`, slow agent, read timeout), the answer goes by its timeout ahead of the re-read lines of its turn and, as the first stream message after the rewind, carries `restart` (reopens the topic). Its turn end then lets nothing else go (`Live::answered_early`). Not changed: keeping it held longer needs a second, hard cap and trades lateness for order.
- I4: a streamed answer is `Op::Stream`, so a permission prompt of the same topic may overtake a queued answer (stream lines already yield to prompts).
- I5: every rewind raises the answer's `until` to `retry + hold` again; while Telegram keeps refusing a line of the segment the answer is pushed back each cycle (no loss; the lines are stuck as well). The "hold + one rewind cycle" bound holds per refusal, not in total.
- I6: an answer that prefers a file (> 4 parts) goes as a document outside the stream and is not gated by `broken`; after a refusal it can show before its turn's lines.
- I6 addendum (fixer round 2, QA BUG-1): the document, a blank `Stop` and an answer dropped by the 256 cap still go outside the stream, but their paired turn end is now recorded as answered (`Live::answered_outside`, called from `answer_ops`), so a rewind that reads it again lets nothing go and leaves nothing for the next `Stop` to claim.
- Timeout before pairing (fixer round 2, checked by code, not fixed): an answer released by its `hold_answer` timeout before any turn end was paired with it (`end: None`) leaves no mark. When its turn end is read later, `Live::turn_end` gives that end to the next held answer (released right after the previous turn's lines, ahead of its own), or leaves it unclaimed for the next `Stop`, which is then released at once. The shift repeats each turn while the next `Stop` arrives after the previous turn end was read, and ends when a `Stop` finds nothing to claim or the unclaimed end lapses. Nothing is lost; only order. A counter of owed turn ends would fix the 1:1 case, but it assumes one turn end per `Stop`: a blocking user Stop hook (`stop_hook_active`) gives several `Stop`s per turn, and a `Stop` whose turn end never reaches the file would make every later answer wait its full timeout. Not small, left as TASK-016 behaviour.

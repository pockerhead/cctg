# FIX_SUMMARY — TASK-023 (fixer)

Preflight: the most dangerous verbatim suggestion was I2's "drop with the existing overflow warning" in `stream_answer`. By that point `answer_ops` has already put the answer's entries into `Live.waiting`. Dropping the ops there leaves `Waiting` entries forever: `unanswered() > 0`, so the stream is never `stuck()`, never rewinds, and the offset never moves again. Checked in `slots.rs` `release` → `answer_ops` → `stream_answer`. The cap is therefore checked before anything is tracked (see I2).

## 1. Fixed

**I1 (major), confirmed.** Answers were paired with re-read turn ends by FIFO order (`Step::TurnEnd => live.held.pop_front()`). The fix pairs them by the transcript byte of the turn end:
- `stream.rs`: `Held.end: Option<u64>` is set when an answer is paired. That happens at `Live::turn_end(line.end)` in `on_chunk`, and on the claim path, where `claim_end` now returns the claimed byte and `ends_unclaimed` is a `VecDeque<u64>`.
- `Live.answered_ends`: `advance` records the end of each accepted answer it pops and prunes those ends once a barrier passes them. `rewind` adds the ends of accepted answers that are still waiting and keeps only the ends it will read again (`> offset`). A re-read turn end found there lets nothing go and is not claimable, so there is no stale claim for the next `Stop`.
- `turn_end` releases the answer with exactly this end first. Otherwise it releases the front answer if that answer is unpaired or overdue, otherwise it records the end as unclaimed.
- `Live::overdue(from)` (called at the start of `on_chunk`): a re-held answer whose turn end is at or before the read start (its lines are committed) goes first, not at the next turn's end.
- `Live::answered_early` (pump timeout path): a re-held answer that goes by timeout before its turn end is read keeps that end, so the late re-read does not release another answer there. `rewind` drops that mark again if the answer is held once more.
- `rewind` keeps unclaimed ends before the offset (they are not read again) instead of clearing all of them.
- Regression tests: `a_turn_end_read_again_lets_only_its_own_answer_go` (the reviewer's probe as an ordering assertion via `Live::answer_numbers`, `#[cfg(test)]`: "second" is message 3 after `> two`, no stale unclaimed end, the next Stop is held), `an_answer_held_again_whose_turn_end_is_committed_goes_first_on_the_re_read`, and `stream.rs` `an_answer_gone_by_its_timeout_keeps_its_turn_end`. The existing test `an_answer_behind_a_refused_line_is_held_again_and_follows_the_re_read_lines` is adjusted to the `VecDeque` type.

**I2, confirmed (the `MAX_HELD` overflow and timeout paths pushed to the scheduler without a cap).** `answer_ops(live, held, room)` returns `Err(held)` when the answer's chunks do not fit in `MAX_QUEUED_MESSAGES - queued`, before any entry is created. `release` then goes to `send_text`, which drops the answer with the single overflow warn. `on_chunk` passes its running `queued`. Test: `a_streamed_answer_past_the_message_cap_is_dropped_untracked`.

Mutations (`scratch/fixer/mutate.py`, output `scratch/fixer/mutations.out.txt`, files restored byte for byte): M1 (no skip of answered ends), M2 (no overdue), M3 (no room check), M4 (advance does not record), M5 (no answered_early), M6 (no retain in rewind). All 6 were KILLED.

## 2. Skipped / documented

- **I3**: documented in IMPL_SUMMARY §5, not fixed. Keeping the answer held past `retry + hold` until its re-read needs a second hard cap and trades bounded lateness for order. The part that was cheap is fixed: `answered_early` stops the late re-read end from releasing another answer. The `rewind` doc comment now says the timeout can put the answer ahead of its lines.
- **I4, I5, I6**: documented in IMPL_SUMMARY §5 and in the PCTX proposal. No code changes, as the orchestrator scoped.
- Review "missing coverage": the session end with an in-flight refused answer and the partial refusal of a multi-part answer have no new tests. Both are outside the fix scope, and reading the code shows no change in behavior there.
- Nits: the `MAX_HELD` doc is unchanged (a rewind can still leave more than 8 held for a moment, which is harmless). The `rewind` doc is updated.

## 3. Test results

One `CARGO_TARGET_DIR=%TEMP%\cctg-023-fix-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`. The directory was deleted afterwards.
- `cargo fmt --all -- --check`: rc=0 (`scratch/fixer/fmt.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: rc=0 (`scratch/fixer/clippy.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0. cctg lib 374 passed / 1 ignored (370 + 4 new), stream_e2e 7, all other binaries green (`scratch/fixer/workspace_test.out.txt`).
- `cargo test -j 1 -p cctg --test stream_e2e` ×3: 7/7 each time (`scratch/fixer/stream_e2e_x3.out.txt`).

## Round 2 (QA NO_SHIP)

Preflight: the riskiest verbatim suggestion in QA BUG-1 was the conditional variant "`live.answered_ends.push(end)` when `read_at < end`, the same condition as in `answered_early`". Checked in `slots.rs`: on the claim path (`on_turn_answer` -> `claim_end`) and on the `Action::Release` path of `on_chunk` the barrier has already moved `read_at` past the end, so the condition is false exactly there, while the committed offset can still be before the end and a rewind reads it again. So the end is recorded unconditionally (deduplicated). Stale entries are harmless: `advance` prunes them at the next barrier and `rewind` keeps only ends it reads again.

### Fixed
- **BUG-1, confirmed** (red first: `scratch/fixer2/doc_e2e.before.out.txt`, `ANS-3` before `> t3`, the same trace as QA). New `Live::answered_outside(&Held)` in `stream.rs` records the paired end in `answered_ends` (no duplicates). `answer_ops` in `slots.rs` calls it on each path where the answer does not ride the stream: blank, file / over the cap (`Err`), empty split.
- Tests: both QA e2e tests ported into `crates/cctg/tests/stream_e2e.rs` as `e2e_a_document_answer_read_again_does_not_let_the_next_answer_go_early` (red before the fix, green after) and `e2e_turns_with_refusals_under_a_slow_telegram_keep_their_answers_in_place`. The fake's `topic_lines` also records accepted documents as `[document]`; the helpers `dump`, `accepted_count`, `stop` and `turn` were added. Unit test `a_turn_end_answered_outside_the_stream_is_not_claimable_when_read_again` in `stream.rs`.

### Checked, documented, not fixed
- **Timeout before pairing (`end: None`)**: real by code (`pump_streams` -> `answered_early` does nothing without an end; the later `turn_end` gives the end to the next held answer, or leaves it unclaimed). Not a small fix: a counter of owed ends assumes one turn end per `Stop`, which fails with several `Stop`s per turn (`stop_hook_active`) and turns a missing end into a timeout for every later answer. Written up in IMPL_SUMMARY section 5.

### Test results (round 2)
One `CARGO_TARGET_DIR=%TEMP%\cctg-023-fix2-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, deleted afterwards.
- `cargo fmt --all -- --check`: rc=0 (`scratch/fixer2/fmt.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: rc=0 (`scratch/fixer2/clippy.out.txt`).
- `cargo test -j 1 -p cctg --test stream_e2e e2e_` x3: 9/9 each time (`scratch/fixer2/stream_e2e_x3.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0. cctg lib 375 passed / 1 ignored, stream_e2e 9, all other binaries green (`scratch/fixer2/workspace_test.out.txt`).


## Round 3 (QA2 NO_SHIP)

Preflight check. The QA claim that could break correct code if applied blindly: "drop the `swap_remove`, the mark stays bounded". Risk: a mark that is never consumed survives a transcript reset (`slots.rs` reset path sets `read_at = Some(0)` without clearing `answered_ends`) and swallows a new turn end at the same byte. Checked: the reset path kept the marks before this change too, and the old code swallowed that byte once as well. One file has only one turn end at a given byte, so keeping the mark adds no new swallow. Bounds hold: `advance` drops marks with `end <= to` once a barrier passes, and `rewind` keeps only `end > offset`. The claim holds and the fix went in as proposed. Reset/rotation stays with the known follow-up (AC3).

### Fixed
- **BUG-2** (`crates/cctg/src/hub/stream.rs`, `Live::turn_end`): a re-read of an answered turn end no longer removes its mark (`contains` + `return None`), so a second rewind to the same barrier still knows that end.
- Regression tests:
  - unit `hub::stream::tests::a_turn_end_read_again_by_two_rewinds_is_not_claimable`: accepted answer, then two refuse -> rewind -> re-read cycles to the same barrier; neither re-read lets anything go or leaves it claimable. After that a barrier past the end drops the mark (bound check).
  - e2e in `crates/cctg/tests/stream_e2e.rs` (ported from `scratch/qa2/qa2_tests.rs.txt`, renamed to the file's `e2e_` style, sessions 10/11, helper `check_answers`): `e2e_a_turn_end_read_again_by_two_rewinds_does_not_let_the_next_answer_go_early`, `e2e_six_turns_with_refusals_keep_every_answer_in_place`.
- Before/after: `scratch/fixer3/unfix.py apply` puts the old `swap_remove` back. With it the unit test and both e2e tests FAIL (`scratch/fixer3/prefix.out.txt`). With the fix they pass.

### Skipped
- None in scope. Known limitations stay as recorded: the unpaired timeout release (`end: None`) and rotation loss both go to the follow-up.

### Test results
One `CARGO_TARGET_DIR=%TEMP%/cctg-023-fix3-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, deleted afterwards.
- `cargo fmt --all -- --check`: rc=0 (`scratch/fixer3/fmt.out.txt`)
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: rc=0 (`scratch/fixer3/clippy.out.txt`)
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0. cctg lib 376 passed / 1 ignored, stream_e2e 11/11, all other binaries green (`scratch/fixer3/workspace_test.out.txt`)
- `cargo test -j 1 -p cctg --test stream_e2e` x3: 11/11 each time, ~13.4 s (`scratch/fixer3/stream_e2e.run{1,2,3}.out.txt`)
- Note: the first workspace run after restoring the fix used a stale build. The restored file had an older mtime than the mutated copy, so cargo did not rebuild. After `touch` the run was green. The results above come from the rebuilt run.

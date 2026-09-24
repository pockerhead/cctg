# QA_REPORT — TASK-023 (small-fix, QA)

## 1. Environment

- Direct: cargo workspace on branch `fix/answer-after-refused-lines` (HEAD f66f9c8), no docker, no real Telegram, no `.env`/`device.env`, nothing written to `~/.claude.json`, no interactive claude, no windows.
- One target dir: `CARGO_TARGET_DIR=%TEMP%\cctg-023-qa-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time. Deleted at the end.
- My e2e test `qa023_e2e.rs` was placed temporarily in `crates/cctg/tests/`, run, copied to `scratch/qa/qa023_e2e.rs.txt` and removed from the tree (`git status` clean for `crates/`). It is the `stream_e2e.rs` harness (real `cctg agent` binary as its own process, real TCP to real `serve_agents`, real `Slots` actor and `Scheduler`, fake Telegram transport) plus two QA tests; the fake additionally records accepted documents as `[document]`.

Reproduce: copy `scratch/qa/qa023_e2e.rs.txt` to `crates/cctg/tests/qa023_e2e.rs`, then `cargo test -j 1 -p cctg --test qa023_e2e -- --nocapture`. Mutations: `python scratch/qa/mutate.py <name> <cargo test args>` (restores the file byte for byte).

## 2. Test results

| Check | Result | Evidence |
|---|---|---|
| `cargo fmt --all -- --check` | rc=0 | `scratch/qa/fmt.out.txt` |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | rc=0 | `scratch/qa/clippy.out.txt` |
| `cargo test -j 1 --workspace --no-fail-fast` | all existing binaries green: cctg lib 374 passed / 1 ignored, stream_e2e 7/7, all other binaries green. rc=101 only because my QA file (picked up in the same run) had the failing counter-example test | `scratch/qa/workspace_test.out.txt` |
| QA e2e `qa_multi_turn_refusals_slow_telegram` x3 | 3/3 pass | `scratch/qa/qa_e2e_x3.out.txt` |
| QA e2e `qa_document_answer_then_refusal_next_answer_waits_for_its_lines` x4 | 4/4 FAIL (deterministic) | `scratch/qa/qa_e2e_x3.out.txt`, `workspace_test.out.txt` |
| `a_new_agent_process_goes_on_from_the_stream_position` x10 | 10/10 pass, 0.56-0.60 s each | `scratch/qa/ac4_determinism.out.txt` |
| Mutation R11 against that test x3 | KILLED 3/3 | same file |
| Mutations against my multi-turn e2e | A (answers ungated, old behaviour) KILLED; C (FIFO pairing) KILLED; B (no skip of answered re-read ends) SURVIVED in the e2e (it is killed by the fixer's unit test `a_turn_end_read_again_lets_only_its_own_answer_go`) | `scratch/qa/mutations.out.txt` |

### My e2e `qa_multi_turn_refusals_slow_telegram`
Real agent + `serve_agents`; bucket capacity 1, refill 250 ms (slow Telegram, lines merge). Five turns:
- turns 1 and 2 written in one append, both Stops sent before the file (two turn ends in one read chunk, held path);
- turns 3 and 4 written in one append, Stops after the file (claim path, two unclaimed ends);
- turn 5: Stop first, file 300 ms later.
Refusals (502, once each): a tool line of turn 1, the answer of turn 2, the prompt line of turn 3, a tool line of turn 4, the answer of turn 5. The trace shows four rewinds, each back to `> t1` (the chunk barrier), and each answer re-held and re-paired correctly. Asserts: exactly 5 refusals; first appearances equal `> warm, > t1, t1c0..2, ANS-1, > t2, ..., ANS-5`; every answer accepted exactly once; every appearance of an answer after at least one appearance of each line of its turn. Pass 3/3; mutations A and C make it fail, so it is not vacuous.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| After a refused stream message the turn's Stop answer comes after all tool lines of that turn (e2e through real `serve_agents`) | `e2e_answer_after_a_refused_line_still_follows_its_lines` (author) and my `qa_multi_turn_refusals_slow_telegram` pass; my `qa_document_answer_then_refusal_next_answer_waits_for_its_lines` FAILS: after a refusal, the next turn's answer shows before its prompt and tool lines (BUG-1) | **FAIL** (main path passes, one reproducible path breaks the order) |
| Answer not delayed beyond `hold_answer` + one rewind cycle, not lost | author e2e bound check (`hold + retry + 3 s`) passes; in my tests no answer was lost and each was accepted exactly once. Repeated refusals push the answer back once per rewind (documented I5) | PASS (with documented I5) |
| Rotation during a refusal checked, result recorded | implementer probe `scratch/implementer/rotation_probe.*` read and its logic checked against `pump_streams` (`slots.rs:1267-1278`: a non-current session's `Live` is removed once `unanswered()==0`, so refused lines are never re-read). Loss confirmed, not fixed, recorded in IMPL_SUMMARY and PCTX proposal. Note: the proposed follow-up task is not created in `maw/tasks/pending/` yet | PASS (recorded; follow-up still to be filed) |
| `a_new_agent_process_goes_on_from_the_stream_position` does not depend on timing | 10/10 runs pass; R11 mutation killed 3/3; the test waits on `gate.parked` and not on sleeps | PASS |
| Existing tests pass | full workspace, fmt, clippy | PASS |

## 4. Bugs found

### BUG-1 (medium-low): an answer sent outside the stream leaves its turn end unpaired; after a rewind the next answer goes out before its own lines

Disconfirmation target (written before searching): "an answer that does not ride the stream (document, over the cap, blank) leaves no mark in `answered_ends`, so when a rewind reads its turn end again, that end becomes an unclaimed end, and the next Stop claims it at once." **The counter-example held.**

Code: `answer_ops` (`slots.rs:2435-2462`) returns `Err(held)` for a document or over-cap answer and `Ok(vec![])` for a blank one. In both cases nothing goes into `Live.waiting`, so `advance` never records `held.end` in `answered_ends`. After a rewind the re-read `Live::turn_end(end)` (`stream.rs:279-300`) does not find the end in `answered_ends`, and the front held answer belongs to a later end (`known <= end` is false), so the end is pushed to `ends_unclaimed`. A later `NewTurn` in the same re-read only starts its `hold_answer` lapse. The next `Stop` within that window gets it from `claim_end`, and `release` streams the answer right away.

Reproduction (e2e, real agent + `serve_agents`, fast bucket; `scratch/qa/qa023_e2e.rs.txt`, test `qa_document_answer_then_refusal_next_answer_waits_for_its_lines`):
1. Warm up the stream. Arm a one-time 502 for `> t2`.
2. Send Stop(big answer of about 25 000 chars, more than 4 parts, so it goes as a document) and Stop("ANS-2"). Then append turn 1 (prompt, 1 call, the big answer) and turn 2 (prompt, 1 call, "ANS-2") in one write.
3. Wait for "ANS-2". Send Stop("ANS-3"), then 500 ms later append turn 3 (prompt, 1 call, "ANS-3").

Expected: `..., > t2, t2c0 ✓, ANS-2, > t3, t3c0 ✓, ANS-3`.
Actual (4/4 runs): `..., > t2, t2c0 ✓, ANS-2, ANS-3, > t3, t3c0 ✓`. ANS-3 is released immediately and is paired with turn 1's end. Nothing is lost, only the order is wrong.

The same hole exists for a blank `Stop` that takes a turn end, and for an answer dropped by the 256 cap (`Err` path). On `main` the problem was wider: every re-read turn end became unclaimed. So this is a leftover of review I1, not a new regression. Triggers: a document-size answer (more than 4 Telegram messages) in the rewind segment, a 5xx/network refusal later in that segment, and the next Stop arriving within `hold_answer` of the re-read. The next Stop usually comes before its transcript lines, so the last condition is the normal case.

Suggested fix (small): when an answer paired with a turn end (`held.end = Some(end)`) does not ride the stream (blank → `Ok(vec![])`, document/over cap → `Err`), record that end as answered right away (for example `live.answered_ends.push(end)` when `read_at < end`, the same condition as in `answered_early`, or unconditionally, since `advance` prunes by barrier and `rewind` filters by `read_again`). Then the re-read end lets nothing go and is not claimable. Regression test: the e2e above (expected order), or a unit test in the slots harness modelled on `a_turn_end_read_again_lets_only_its_own_answer_go` with a >4-part first answer.

### Observations (not TASK-023 regressions)
- Under slow Telegram with refusals, a rewind goes back to the barrier at the end of the last fully accepted chunk. Because reads are large, each refusal re-sent `> t1` and every later line again (4 rewinds gave 4 copies of turn 1 in my trace). This follows TASK-016's at-least-once design, but it gets noisy in a real topic when refusals repeat.
- A never-paired answer released by its `hold_answer` timeout (`end: None`) is not recorded by `answered_early`. Its turn end, read later, then goes to the next held answer or stays unclaimed. This is TASK-016 behaviour kept by this task (the fixer covered only the `end: Some` case). I did not reproduce it end to end.

## 5. Verdict

**NO_SHIP.** The main fix works. Answers ride the stream, wait behind refused lines, are re-held on rewind and paired by transcript byte. My own multi-turn, slow-Telegram, five-refusal e2e passes 3/3 and is not vacuous (mutations A and C are killed). AC2 to AC5 pass. But AC1 ("after a refusal the answer comes after its turn's lines") fails on a reproducible path through the real `serve_agents` (BUG-1). It is the same class as review I1, reached through answers that bypass the stream tracking. The fix is a few lines plus the ready regression test. One fixer round should close it.

Cleanup: no services started. The QA test file was removed from `crates/`. `%TEMP%\cctg-023-qa-target` was deleted after the report.

# QA_REPORT_2 — TASK-023 (small-fix, QA round 2)

## 1. Environment

- Direct cargo workspace, branch `fix/answer-after-refused-lines`, HEAD e46c41c. No docker, no real Telegram, no `.env`/`device.env`, nothing in `~/.claude.json`, no interactive claude, no windows.
- One target dir `%TEMP%\cctg-023-qa2-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time. Deleted at the end.
- My e2e tests (`scratch/qa2/qa2_tests.rs.txt`) were appended to a copy of `crates/cctg/tests/stream_e2e.rs` as `crates/cctg/tests/qa2_023_e2e.rs`, run, then removed. The copy gives the same harness: the real `cctg agent` binary as a child process, real TCP to the real `serve_agents`, the real `Slots` actor and `Scheduler`, and a fake Telegram transport.

Reproduce: `cat crates/cctg/tests/stream_e2e.rs scratch/qa2/qa2_tests.rs.txt > crates/cctg/tests/qa2_023_e2e.rs`, then `cargo test -j 1 -p cctg --test qa2_023_e2e qa2_ -- --nocapture --test-threads 1`. Mutations: `python scratch/qa2/mutate.py <name> <cargo args>`. The script restores the file byte for byte.

## 2. Test results

| Check | Result | Evidence (`scratch/qa2/`) |
|---|---|---|
| `cargo fmt --all -- --check` | rc=0 | `fmt.out.txt` |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | rc=0 | `clippy.out.txt` |
| `cargo test -j 1 --workspace --no-fail-fast` | rc=0: cctg lib 375 passed / 1 ignored, stream_e2e 9/9, all other binaries green | `workspace_test.out.txt` |
| BUG-1 e2e `e2e_a_document_answer_read_again_does_not_let_the_next_answer_go_early` | passes at HEAD. Mutation `no_answered_outside` (the three `answered_outside` calls removed) makes it FAIL with the QA round 1 trace (`ANS-2, ANS-3, > t3`). It really drives `start_hub` -> `serve_agents` with the real agent | `mut_no_answered_outside.out.txt` |
| my `qa2_turn_end_read_again_by_two_rewinds_is_not_claimable` | **FAIL 3/3** | `qa2_e2e.run{1,2,3}.out.txt` |
| my `qa2_six_turns_refusals_slow_telegram` | **FAIL 3/3** | same |
| both of mine with candidate fix `keep_answered_mark` | PASS. With it, lib 375, stream_e2e 9 and qa2 11 all pass | `candidate_fix.out.txt`, `candidate_fix_suite.out.txt` |
| mutation `answers_plain` (answers go as `Op::Send` again, the pre-task behaviour) on my six-turn test | FAILS, so the test also sees the original bug | `mut_answers_plain.out.txt` |
| `a_new_agent_process_goes_on_from_the_stream_position` x10 | 10/10 pass, 0.57-0.58 s | `ac4_x10.out.txt` |

## 3. Disconfirmation (written before the search)

Counter-example: **the `answered_ends` mark of an accepted answer is used up by the first re-read of its turn end (`Live::turn_end` does `swap_remove`). A second rewind to the same barrier no longer knows that end. The third read then leaves it as an unclaimed end, and the next `Stop` claims it and goes out before its own lines.** Only the first rewind is covered: the rewind rebuilds `answered_ends` from accepted answer entries in `waiting`, and after the first re-read nothing holds that end any more.

**It held.** Reproduced through the real `serve_agents` path with the real agent, 3/3.

## 4. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| After a refused stream message, the turn's Stop answer comes after all tool lines of that turn (e2e through `serve_agents`) | The author's e2e tests pass. The BUG-1 e2e passes and is not vacuous. My two e2e tests fail: when two refusals rewind over an already answered turn end, the next answer goes out before its prompt and tool lines (BUG-2) | **FAIL** |
| Not delayed beyond `hold_answer` + one rewind cycle, not lost | No answer lost in any run. In every run each answer was accepted exactly once, and every planned refusal happened. Repeated refusals push the answer back (I5, documented) | PASS |
| Rotation during a refusal checked, result recorded | The implementer's probe and reasoning (IMPL_SUMMARY section 1, PCTX proposal): lines are lost, not fixed, follow-up proposed | PASS (recorded; the follow-up task still has to be filed) |
| `a_new_agent_process_goes_on_from_the_stream_position` is timing-independent | 10/10, the test waits on a gate condition | PASS |
| Existing tests pass | fmt, clippy, full workspace | PASS |

## 5. Bugs found

### BUG-2 (medium): an answered turn end that is read again by a second rewind becomes claimable; the next answer goes out before its lines

Code: `crates/cctg/src/hub/stream.rs` `Live::turn_end`:
```rust
if let Some(at) = self.answered_ends.iter().position(|&known| known == end) {
    self.answered_ends.swap_remove(at);
    return None;
}
```
`Live::rewind` rebuilds `answered_ends` from the old marks (filtered by `> offset`) and from accepted answer entries still in `waiting`. After the first re-read, the mark is gone and no entry carries that end. So when a refusal later in the same segment causes a second rewind to the same barrier, the end is forgotten. On the third read, `turn_end(E1)` finds no mark and no matching held answer, and pushes E1 to `ends_unclaimed`. A `NewTurn` later in the read only starts the lapse (`hold_answer`, 5 s). The next `Stop` claims E1 in `on_turn_answer` and is released at once.

Reproduction 1 (`qa2_turn_end_read_again_by_two_rewinds_is_not_claimable`, fast bucket):
1. Warm up the stream. Arm two one-time 502s for `> t2`.
2. Send Stop(ANS-1) and Stop(ANS-2). Append turn 1 (prompt, 1 call, ANS-1) and turn 2 in one write.
3. After ANS-2 shows, send Stop(ANS-3). Append turn 3 500 ms later.

- Expected: `> warm, > t1, t1c0, ANS-1, > t2, t2c0, ANS-2, > t3, t3c0, ANS-3`
- Actual (3/3): `..., > t2, t2c0 ✓, ANS-2, ANS-3, > t3, t3c0 ✓`

Reproduction 2 (`qa2_six_turns_refusals_slow_telegram`, bucket 1 per 200 ms, six turns, refusals `t1c0, ANS-1, t2c1, ANS-4, > t5, t6c0`). Four rewinds go back to `> t1`. ANS-5 is released right after ANS-4, before `> t5`. The shift then repeats: ANS-6 claims the turn 5 end left free and shows before `t6c0`. Two answers out of place in one realistic run, 3/3. Nothing is lost, and every answer is accepted once.

The trigger is ordinary: two or more refusals inside one uncommitted segment. Under a slow Telegram each read covers several turns, and the QA round 1 trace already showed four rewinds back to the same barrier. Add a `Stop` that comes before its lines, which is the usual order. This is the same AC1 class as review I1 and BUG-1, and the multi-refusal e2e from round 1 simply did not send a `Stop` inside the lapse window after the repeated rewinds.

Fix (verified as a candidate, not committed): do not consume the mark on re-read. Replace the `swap_remove` with just `return None`. `advance` already prunes marks once a barrier passes them (`end > to`), and `rewind` keeps only ends it will read again, so the mark stays exactly as long as that end can be read again. With this change my two tests pass, and so do lib 375, stream_e2e 9 and the BUG-1 e2e (`candidate_fix_suite.out.txt`). Suggested regression tests: my two e2e tests (port them into `stream_e2e.rs`), plus a `Live` unit test: accepted answer, rewind, re-read, second rewind, re-read, then `ends_unclaimed` is empty.

### Left open (not blocking, per the orchestrator)
- The unpaired timeout release (`end: None`, IMPL_SUMMARY section 5) is the known limitation going to a follow-up task. I did not target it. My failures come from BUG-2: the candidate fix alone turns them green.
- Rotation loss (AC3): recorded, and the follow-up task still has to be created in `maw/tasks/pending/`.

## 6. Verdict

**NO_SHIP.** BUG-1 is closed. Its e2e drives the real `serve_agents` path and fails under the mutation that removes the fix. AC2 to AC5 pass. But AC1 fails on a second reproducible path (BUG-2): two refusals that rewind over an already answered turn end let the next turn's answer out before its lines, deterministically 3/3 in two independent e2e tests. The fix is one line, and it has been checked against the whole cctg suite.

Cleanup: no services started. `crates/cctg/tests/qa2_023_e2e.rs` was removed (`git status` shows only `scratch/qa2/`). `%TEMP%\cctg-023-qa2-target` was deleted.

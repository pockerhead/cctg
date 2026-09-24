# IMPL_REVIEW — TASK-023 (small-fix, code-reviewer)

## 1. Verdict

**NEEDS_WORK**: the main path is fixed and tested, but a re-held answer is paired with the next re-read turn end by FIFO order. A rewind window that holds an earlier turn end whose answer was already accepted lets a later turn's answer out before its own lines. This is the AC1 ordering the task is meant to fix, and a probe confirms it.

## Disconfirmation (done first)

Counter-example to find: "after a refusal the answer of turn N still shows before turn N's lines". The implementer's claim is "the re-read turn end lets it go after the re-sent lines". That holds only if the first turn end re-read after the rewind belongs to the first re-held answer.

Case tested: one chunk with `> one`, TurnEnd1, `> two`, TurnEnd2, and both Stops held before the read. Telegram accepts `> one` and "first" and refuses `> two` (502). "second" is dropped unsent behind it, and the rewind goes back to 0 with `held = ["second"]`. The re-read pops "second" at **TurnEnd1**, between `> one` and `> two`. TurnEnd2 then finds nothing held, so `ends_unclaimed = 1`.
Result: **the counter-example held (bug confirmed)**. Probe: `scratch/reviewer/probe_mispair.rs.txt`, output `scratch/reviewer/probe_mispair.out.txt` (`held=0 ends_unclaimed=1 unanswered=3`). It ran in a git-archive copy of HEAD, and the copy is deleted.

## 2. Confirmed correct

- The answer no longer bypasses the `broken` gate. `answer_ops` (`crates/cctg/src/hub/slots.rs:2415-2446`) turns it into `Op::Stream { merge: false, restart: take(live.restart) }`. The scheduler drops it unsent behind a refused line (`scheduler.rs:420-431`, `555-572`). `merge_lines` stops at a `merge: false` job (`scheduler.rs:597-604`), so no line merges into an answer and the answer never merges into lines.
- No loss on the main path. `Live::rewind` (`stream.rs:375-404`) re-holds `Waiting|Refused` answer entries ahead of the held ones, with `until >= at + hold`. Accepted answers are not re-held, so there is no duplicate beyond the documented whole-answer resend when a later part of a multi-part answer is refused.
- Rewind happens only when `stuck()` (nothing in flight), so no in-flight answer entry is discarded.
- A session that ends with answer entries in flight keeps its `Live` until `unanswered() == 0` (`slots.rs:1264-1275`). A later rewind re-holds the answer, and the next pump releases it via `send_text` (`Op::Send`, since `stream_target` is `None`). `send_text` has no current-slot check (`slots.rs:1634-1660`), so the answer is not lost.
- TASK-022 (sessions without a stream) is untouched: `on_turn_answer` and `release` fall back to `send_text` when `stream_target` is `None` (`slots.rs:1135`, `1176-1194`).
- The claim path (turn end read before the Stop) now goes through `release()` and so through the stream (`slots.rs:1142-1147`). The unit test `an_answer_behind_a_refused_line_is_held_again_and_follows_the_re_read_lines` covers claim, refusal, re-hold and re-read.
- The e2e `e2e_answer_after_a_refused_line_still_follows_its_lines` (`tests/stream_e2e.rs:715-802`) goes over a real `cctg agent` and `serve_agents`. It checks the order of first appearances, that the answer is sent once, that both refusals happened, and the time bound `hold + retry + 3 s`.
- `a_new_agent_process_goes_on_from_the_stream_position` is now structurally deterministic. The `gated_reader` parks a read, so `live.reading == Some(conn 1)` is guaranteed at disconnect. The test waits on conditions, and the only time bound (5 s) sits below `READ_TIMEOUT` (10 s).
- AC3 (rotation during a refusal): the result is recorded with a probe and a reason (`scratch/implementer/rotation_probe.*`), and a follow-up task is proposed. That meets the AC ("тест или обоснование").
- I re-ran the checks: `cargo clippy -j 1 --workspace --all-targets -D warnings` is clean on the real repo. `cargo test --workspace` passes: lib 371+probe, stream_e2e 7, all other binaries green. The one `hook_cli` failure came from my shared target dir (a binary built in the copy), and the test passes after a rebuild (`scratch/reviewer/clippy.out.txt`, `workspace_test.out.txt`). No new crates.

## 3. Issues

### I1 (major): a re-read turn end of an already answered turn takes a later turn's answer
`crates/cctg/src/hub/slots.rs:1479-1494` (`Step::TurnEnd => live.held.pop_front()`) together with `crates/cctg/src/hub/stream.rs:382-400` (`rewind` puts re-held answers at the front of `held` and resets `ends_unclaimed`).
The rewind goes back to the last fully accepted barrier. Any turn end between that barrier and the refused message whose answer was accepted gets re-read too. It pops the front of `held`, which is a later turn's re-held (or still held) answer. That answer is queued right after the earlier turn's lines, before its own lines (confirmed by the probe). Two follow-on effects:
- The later turn's own end then counts as `ends_unclaimed`, and the next `Stop` can claim that stale end at once (`claim_end`). The next answer then goes out before its turn's lines are read.
- The wrong pairing also lines up with a claimed answer whose turn end sits before the rewind barrier. That answer is re-held and then released at the *next* turn's end, i.e. after the next turn's lines. Late rather than early, but still wrong.

The trigger is two turn ends in one read segment. That is more likely exactly when Telegram is refusing or slow, because reads pause at `STREAM_QUEUE`/`MAX_WAITING` and chunks get larger. Quick channel turns without tools make it easy to reach.
Suggested fix: pair answers with their turn end by transcript position, not by count. Store the turn-end byte (`line.end`) on the `Held` when it is popped in `on_chunk`, and for the claim path keep the claimed end's offset (for example `ends_unclaimed` as a small `VecDeque<u64>`). On a re-read TurnEnd at offset X: if the answer for X was accepted (keep the accepted ends ≥ the rewind offset in `rewind`), skip it and count it neither as a release nor as unclaimed. Pop a held answer only when its recorded end is X or unset. Add the probe above as a regression test, asserting the order `> one, first, > two, second` (or `held`/`ends_unclaimed` state as in the probe).

### I2 (minor): a streamed answer is no longer capped, and the hold overflow path is not bounded
`slots.rs:1150-1157` → `release` → `answer_ops` → `stream_answer` (`slots.rs:1197-1213`) bumps `queued_messages` but never checks `MAX_QUEUED_MESSAGES`. The claim and `on_chunk` paths are indirectly bounded by the read budget. The `MAX_HELD` overflow path is not: while Telegram stalls (a long `retry_after`, a hanging send), reads stop, every Stop past 8 pushes the oldest held answer into the scheduler's unbounded lane, and nothing drops it. The summary says "the stream bounds itself per session", which is not true for this path. `unanswered()` can grow past `MAX_WAITING`, and that only stops reads. The normative TASK-022 bullet says the answer shares the 256 pending-send cap. That has changed for streamed sessions, and `PCTX_PROPOSALS.md` does not mention it.
Fix: when `queued_messages + ops.len() > MAX_QUEUED_MESSAGES`, drop with the existing overflow warning, as `send_messages` does (or fall back to `send_text`, which does). Or state the deviation in the PCTX proposal. The pace is human-scale, hence minor.

### I3 (minor): after a rewind, a held answer can time out ahead of its re-read lines, and it carries `restart`
`slots.rs:1305-1314` plus `answer_ops` taking `live.restart`. If the re-read is delayed (the queue is at `STREAM_QUEUE`, the agent is slow, a read timeout), a re-held answer with `until = retry + hold` is released first. It takes `restart = true`, clears `broken`, and shows before the lines of its turn that will be re-read later. This is within the "hold bound" the spec accepts, but the doc comment in `rewind` ("the read at `at` lets them go after their lines again") oversells it. Document it, or keep the answer held while `live.restart` is still true and a read is pending, up to a hard cap.

### I4 (minor): a permission prompt now overtakes a queued answer of its own topic
`scheduler.rs:409-410`: stream ops do not mark the topic busy in `next_permission`. The next turn's prompt can show before the previous turn's answer. This is documented in the summary but missing from the PCTX proposal. Accept it and record it there, or let an `Op::Stream` with `merge: false` (answers) mark the topic busy.

### I5 (minor): repeated refusals push the answer back without a bound
Each rewind raises `until` to `at + hold` again. As long as Telegram keeps refusing a line of the segment, the answer is re-held every cycle. There is no loss and the lines are stuck too, but AC2 ("не больше hold + один цикл rewind") holds only for a single refusal. Record this in the summary/PCTX.

### I6 (minor): a document answer (>4 parts) is still ungated
`answer_ops` `Err(held)` → `send_text` → `Op::SendDocument`. If the topic is broken, it shows before the turn's lines. This is documented. It is fine as a known limitation, but it belongs in the PCTX proposal.

## 4. Missing coverage

- A regression test for I1: two turn ends in one segment, the earlier answer accepted, a later line refused. Assert the order and `ends_unclaimed == 0` after the re-read.
- A stale unclaimed end after a rewind: the next Stop must not claim it and release its answer before its lines are read.
- Session end (or `/clear`) with an answer entry in flight that is then refused: the answer is still sent (via `send_text`) after the rewind. The code path is right by reading, but no test covers it.
- A multi-part streamed answer with only the last part refused: the whole answer is resent once and the first part at most twice.
- The cap path from I2 (MAX_HELD overflow under a stalled scheduler).

## 5. Nits

- `Entry::Barrier` reformatting by `cargo fmt` is unrelated noise, but harmless.
- The `rewind` doc says "None goes by its timeout before `at + hold`". Correct, but it would help to say that it can still go by timeout *before* its re-read if the read is late (I3).
- `rewind` can leave `held.len() > MAX_HELD`. That is harmless, but the `MAX_HELD` doc says "the oldest goes when one more comes", which no longer strictly holds.

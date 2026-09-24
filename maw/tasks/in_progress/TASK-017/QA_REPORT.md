# TASK-017 QA_REPORT (qa, claude opus, medium)

Verdict: **SHIP**

## 0. Disconfirmation

The counter-example I expected to break it: a slot that lives through a dead period, a hub restart and then revival by each of the three paths (resume, a new session, `/clear`) must deliver every kept message exactly once. Duplicates or a second Resume button after the restart, a nested `claude -p --resume <dead id>` reviving the slot, or kept messages going to the still-open agent of the ended run would each break the task.

Searched in code first (`slots.rs` park/flush/flush_all/offer_resume/on_resume_done/press_resume, `registry.rs` SessionEnd `entry.agent = None`, `session_started`'s early `Parent` return for a nested resume of a top-level id, `follow_pid`, `after_restart`), then ran it as the e2e below. **It held:** no duplicates, no second button, the nested resume does not revive A (A stays `ended`), and the old-run agent and the nested agent get nothing.

## 1. Environment

Direct cargo, no docker, no Telegram, no `.env`/`device.env`, nothing in `~/.claude.json`, no claude process.
One `CARGO_TARGET_DIR=%TEMP%/cctg-t017-qa`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, `--offline`, one cargo at a time. The directory was deleted at the end. No services were started.

Reproduce:
```
copy scratch/qa/qa_buffer_e2e.rs.txt crates/cctg/tests/qa_buffer_e2e.rs
cargo test -j 1 --offline -p cctg --test qa_buffer_e2e
```

## 2. Test results

| Run | Result | Evidence |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | scratch/qa/fmt.txt (empty) |
| `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings` | exit 0 | scratch/qa/clippy.txt |
| `cargo test -j 1 --offline --workspace --no-fail-fast` | exit 0: cctg lib 393 passed / 1 ignored, buffer_e2e 1, stream_e2e 11, hook_cli 7, all other binaries ok | scratch/qa/workspace_test.txt |
| lib test binary `hub::`, 3 parallel processes × 7 rounds (2 of them right after the full workspace run) | 21/21 ok, 297 passed each, no 60 s WAIT, no failing test to name | scratch/qa/load_hub.txt |
| QA e2e `qa_dead_slot_buffer_end_to_end` (new, mine) | 7/7 ok (~3 s each) | scratch/qa/qa_buffer_e2e.rs.txt, qa_e2e_result.txt |
| Mutations against the QA e2e | 4/4 killed | scratch/qa/mutations.txt |

### QA e2e (independent from the author's buffer_e2e)

This is the real `serve_agents` on TCP, the real `Slots` and `Scheduler`, and a fake transport. Topic and message ids are numbered, and the second hub run gets fresh ranges. Raw wire peers stand in for `cctg agent`. Telegram updates go through the real `updates::route_batch` allowlist gate, with a made-up user id. The steps:
1. A starts (pid 10) and its agent links. A live message goes straight through. A ends while the old agent's TCP link stays open.
2. `SubagentStart` of A. A nested new run N (parent pid 30, the live P in another folder), with its own agent linked. A nested `claude -p --resume A` (pid 31, parent 30), then its SessionEnd with pid 31.
3. 52 messages (ids 2..53): the buffer on disk is `4..53`, with exactly 1 `OVERFLOW_NOTICE`, 0 `QUEUED_NOTICE`, 1 Resume button, no Delete. The icon edits to dead (`ICON_DEAD`) and there is no close op. The old agent, the nested agent and the nested resume all get nothing, and A stays `ended`.
4. Every `callback_data` is `resume:<uuid>` and at most 64 bytes. An allowlisted press answers `ANSWER_UNAVAILABLE` and `resume_asked` is saved. A stranger's press produces no `answerCallbackQuery` at all. A garbage `resume:../../x` press is answered and changes nothing. `registry.json` contains neither user id.
5. Restart: the actor, scheduler and server are aborted, then a new hub starts from the saved `registry.json`. There is no second button and no new topic, and the buffer is still `4..53`.
6. Revival by resume: SessionStart(resume, pid 12), then the agent links. It gets exactly `4..53` in order and nothing more in 500 ms. The run-1 button message is edited to `RESUMED_TEXT` and `buffer` disappears from disk. A press now answers `ANSWER_ALIVE`.
7. Second dead period, 51 messages (60..110): one more overflow warning and one new button. A new session B (pid 40) takes slot 0 without a new topic, and its agent gets exactly `61..110`. The ended A agent gets nothing, button 2 is edited away, and an old A press answers `ANSWER_EXPIRED`.
8. Third period, `/clear`: SessionEnd(B, clear, pid 40), two messages, then SessionStart(C, clear, pid 40). The same TCP peer (following its pid) gets `[120, 121]` once, the buffer goes idle, and the next live message goes straight through. There are 2 overflow warnings in total, one for each overflowing period. There are still only 2 slots.

Mutations (each one reverted with `git checkout` afterwards):
- M1: overflow warned on every drop. Killed (count ≠ 1).
- M2: Resume offered on every pump. Killed.
- M3: a full buffer drops the newest. Killed.
- M4: a handed message is not popped (duplicates). Killed.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| A message to a dead slot is buffered, the topic is not closed, and the dead icon shows | QA e2e steps 1-3 (buffer on disk, `EditTopic` with `ICON_DEAD`, no close/delete op) | PASS |
| The 51st message drops the oldest, with exactly one warning per dead period | QA e2e steps 3 and 7 (52 → `4..53`, 1 warning; the next period gives 1 more; total 2); mutation M1/M3 | PASS |
| Revival delivers once, in order, then clears; resume, a new session and `/clear` each revive; nested runs and subagents do not | QA e2e steps 2, 6, 7, 8 (+ mutation M4); author tests in the green lib run | PASS |
| The buffer survives a hub restart without duplicates | QA e2e steps 5-6 (no second button, exact 50 once, idle on disk after) | PASS |
| Resume `callback_data` ≤ 64 bytes with a clear answer while not wired | QA e2e step 4 (all buttons ≤ 64, `ANSWER_UNAVAILABLE`/`ANSWER_ALIVE`/`ANSWER_EXPIRED`) | PASS |
| No Telegram user id in the buffer, its persistence or the logs | QA e2e (registry.json has neither id); `tests/message_logs.rs` (logs) green | PASS |
| Existing tests pass | fmt, clippy -D warnings, full workspace test; `hub::` under load 21/21 | PASS |

## 4. Bugs found

No blocking bugs. Observations (low, not fixed, no action required for ship):
- **Low, residual:** `flush` stops when the agent link queue (64) is full. The rest waits for the next actor event, and with no event that is the 60 s retry tick. Max buffer 50 + the `Registered` frame fits in 64, so this needs extra traffic in the same link. It is not reproducible in normal use.
- **Low, pre-existing:** if a session's SessionEnd hook is lost while the hub is down, the slot stays `NoChannel` after a restart. Messages then get `QUEUED_NOTICE` and wait with no Resume button until another session takes the slot. This comes from the registry (TASK-011), not from this task.
- Documented residuals in PLAN_FINAL §4 (at-least-once across a hub crash, a lost button send, a one-try keyboard edit, old-agent reconnect in the resume window) were not re-tested beyond reading the code. They match the code.
- Live Telegram check: not executed (out of scope, no real bot allowed).

## 5. Verdict

**SHIP.** Every acceptance criterion passes on my own end-to-end test over the real TCP path. The test covers restart, all three revival paths, nested, subagent and stranger cases, and it is not vacuous (4/4 mutations killed). The workspace is clean under fmt, clippy and tests. The `hub::` timing flake did not show up in 21 parallel runs.

Cleanup: the temporary test file was removed from `crates/` (a copy is in `scratch/qa/`), the target dir was deleted, and no services were started.

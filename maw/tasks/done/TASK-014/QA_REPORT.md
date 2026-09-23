# QA REPORT: TASK-014 (permission relay end to end)

Verdict: **SHIP** (criterion 6, the live check, is manual and stays with the orchestrator after merge)

## 0. Disconfirmation

The counter-example I chose: after `SessionEnd` of session B, its agent link is still up and a `permission_request` is already queued on the agent ingress (read by the reader task before or after the end). If the hub shows a prompt with live buttons, or sends a verdict after a press, the fix is wrong. Second variant: a frame read before `/clear` but taken by the actor after the rebind to the new session C must not become C's prompt.

Checked against the real `Slots::run` + real `Scheduler`, with separate hook and agent channels (`qa_relay_end_to_end` step 9, `qa_clear_moves_the_slot_on_and_prompts_follow_their_session`). **The counter-example did not hold.** Both requests (with `received_at` before and after the SessionEnd) are dropped: no prompt, no verdict. The pre-`/clear` frame stays A's (A has ended, so it is dropped), and the next frame lands as C's prompt in the same slot topic. Mutation check: with the `entry.ended` check turned off (in `on_permission_request` and `send_prompts`), both of my tests fail. With `session_at` replaced by the current `conn.session`, the `/clear` test fails. The tests are not vacuous.

## 1. Environment

- No docker-compose or dev server. The project is a Rust workspace, so I ran it directly with `cargo`. Telegram is replaced by a fake `Transport`, agents by `mpsc` links. The real Telegram API and live Claude Code sessions were not used.
- One `CARGO_TARGET_DIR=%TEMP%\cctg-t014-qa-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`. The directory was deleted at the end.
- My tests do not touch repo code. The file is `maw/tasks/in_progress/TASK-014/scratch/qa/qa_relay.rs`. I ran it in a throwaway workspace copy (`git archive HEAD Cargo.toml Cargo.lock crates docs` into `scratch/qa/ws`, the file copied as `crates/cctg/tests/qa_relay.rs`). The copy was deleted after the runs.

Reproduce:
```
export CARGO_TARGET_DIR="$TEMP/cctg-t014-qa-target" CARGO_PROFILE_DEV_DEBUG=0
cargo fmt --all --check
cargo clippy --workspace --all-targets -j 1 -- -D warnings
cargo test --workspace --no-fail-fast -j 1
# QA tests:
mkdir -p /tmp/ws && git archive HEAD Cargo.toml Cargo.lock crates docs | tar -x -C /tmp/ws
cp maw/tasks/in_progress/TASK-014/scratch/qa/qa_relay.rs /tmp/ws/crates/cctg/tests/
(cd /tmp/ws && cargo test -p cctg --test qa_relay -j 1)
```

## 2. Test results

Existing suite (HEAD `3fb512a`):
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast -j 1`: **7 runs**, all green. cctg lib 273 passed / 1 ignored on every run; `permission_logs`, `message_logs`, `slots_logs`, `ingress_logs` and the rest pass. No flakes.
- `git diff main --check -- crates docs Cargo.toml Cargo.lock`: clean. `Cargo.toml`/`Cargo.lock` unchanged.

New QA tests (`scratch/qa/qa_relay.rs`, 7 tests, **5 runs of 7/7 green**):

| Test | What it checks |
|---|---|
| `qa_relay_end_to_end` | Two sessions in two folders with the same id `abcde`: each prompt goes to the topic of its own session. Text ≤ 4096 UTF-16 with a 6000×😀 preview. Buttons are exactly `allow:abcde`/`deny:abcde` (≤ 64 bytes). ❓ icon in both topics. A stranger's press gives `Ignored::NotAllowed`, no answer, no verdict. The first press gives one verdict with `verdict_id`, only to A's agent, and no edit before the ack. The link drops before the ack: the reconnected agent gets the same `verdict_id`. A press before the ack answers `Уже решено` and keeps the first answer. An ack from B's agent for A's id is ignored. A's own ack produces one edit with ✅ and `{"inline_keyboard":[]}`, and A's icon goes back to ⚡️ while B stays ❓. A later press answers `Уже решено` with no verdict, and no resend after the ack even across retry ticks. Deny in B goes only to B. The icon stays ❓ while another prompt of the session is open. SessionEnd gives `Сессия завершилась` with no buttons; the edit fails twice with 500, succeeds on the third try, and no further edits follow. The icon changes to 🏁. A press on the closed prompt answers `Запрос устарел` with no verdict. Late requests after SessionEnd are dropped. |
| `qa_clear_moves_the_slot_on_and_prompts_follow_their_session` | `/clear` in start-first order closes A's prompt. The pre-clear frame is not attributed to C. C's prompt lands in the same topic. An old A button answers `Запрос устарел`. A press on C's prompt goes to the rebound connection. |
| `qa_prompt_overtakes_another_topics_backlog` | Slow bucket (1 send / 150 ms), 10 replies in topic B, then a prompt in topic A: the prompt is at most the third send. |
| `qa_agent_before_session_start_prompt_waits_for_the_topic` | A request from an agent that arrived before SessionStart waits and is shown in the slot topic after the hook. |
| `qa_legacy_agent_is_decided_by_the_hand_off` | An agent without `verdict_ack` gets a verdict without an id. The hand-off decides the prompt, and there is exactly one verdict. |
| `qa_full_agent_queue_answers_offline_then_delivers_on_tick` | A full agent queue answers `Сессия не на связи…`. The verdict arrives on the retry tick. |
| `qa_nested_resume_end_keeps_the_prompt_and_reuse_of_pid_closes_it` | A SessionEnd with a different pid (nested `--resume`) closes nothing. A new top-level session on the same pid closes the old prompt. |

## 3. Checking the fixer's claims (FIX_SUMMARY)

| Claim | Where in the code (e542327) | Result |
|---|---|---|
| No prompt for a known ended session | `slots.rs` `on_permission_request`: `entry.ended` gives drop. `send_prompts`: an ended session gets `finish(Closed)` and is not sent. An unknown session is kept (agent-before-SessionStart). | Confirmed by code and by my tests (step 9, early-agent test) |
| A frame belongs to the session bound when it was read | `ingress.rs`: `received_at` is taken right after `read_line`. `slots.rs`: `Conn.bindings` with rebind time, `session_at` via `rposition(at <= received_at)`. Request, ack and reply go through `frame_session`. | Confirmed. The mutation to `conn.session` is caught by my test |
| Prompts are closed before prune | `registry.rs` `apply_hook(SessionStart)`: snapshot of `(id, ended)` before `session_started`, then `ended_sessions` = gone or newly ended. `SessionEnd` returns `vec![session]` only on the accepted path (the ignored nested one returns `Followup::default()`). `prune` removes only `ended` sessions, so a live session is never closed through the `None` branch. | Confirmed by code. The lib test `start_first_clear_closes_a_prompt_even_when_pruning_removes_its_session` passes |
| Terminal edit errors count as applied | `on_prompt_edit_done`, new test `terminal_permission_edit_errors_are_treated_as_applied` | Confirmed |

## 4. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| `callback_data` ≤ 64 bytes and exactly one matching verdict | `qa_relay_end_to_end` steps 1, 3, 6. Lib `callback_data_fits_and_round_trips` | PASS |
| A stranger's callback sends no verdict and reveals nothing | step 2: `Ignored::NotAllowed`, no `AnswerCallback`, no verdict. `permission_logs`: no request fields or user ids in the logs | PASS |
| A second or late callback is idempotent | steps 4-5 (before and after the ack, retry ticks), closed prompt in step 8 | PASS |
| Long `input_preview` ≤ 4096, and permission traffic overtakes the queue | 6000×😀 gives ≤ 4096 including ✅. `qa_prompt_overtakes_another_topics_backlog` + lib `a_prompt_overtakes_a_full_reply_backlog`. By project law the prompt overtakes other topics' queues, not earlier jobs of its own topic | PASS |
| The prompt goes to the topic of its session's slot even after the slot moved on | `qa_clear_…`, two-session test, lib `an_ended_sessions_prompt_never_reaches_the_topic_its_slot_moved_on_to` | PASS |
| Live: a Telegram answer closes the terminal dialog | Not run: QA is forbidden to start live Claude Code or call Telegram. It is the orchestrator's check after merge (PLAN_FINAL section 4), together with the check that an empty `inline_keyboard` removes the buttons | NOT RUN (manual) |
| Existing tests pass | 7 workspace runs + fmt + clippy | PASS |

## 5. Bugs found

No blockers or majors found. Observations (low, no fix needed for merge):

1. **Low.** The fix drops any `permission_request` whose frame session is already `ended`. If the new session's `SessionStart(clear)` hook is lost (hub briefly down, hook is one-shot), the agent stays bound to the ended A, and requests of the live process are silently dropped (only `debug!`). Before the fix such a prompt would still have been shown. In practice the window is tiny: a new request after `/clear` needs the user's next turn, and the hooks arrive earlier. The terminal dialog keeps working.
2. **Low.** A prompt from an agent of a session that never gets a `SessionStart` stays in the book as unsent until `MAX_PROMPTS` evicts it. This is bounded memory, not a leak.
3. **Info.** A stranger's callback gets no `answerCallbackQuery` at all, so the stranger's client shows a spinner until Telegram's timeout. That is enough for "reveals no details", and it is intended.

Leak hunt: every new `info!/warn!/debug!` carries only `conn`, a short session, `?behavior`, `attempts` and fixed text. `%error` comes from `ApiError` (reqwest without URL, Telegram description) and `WireError` (fixed texts). The prompt text goes only to Telegram. `permission_logs` checks the absence of tool, description, preview, request ids, verdict id and both user ids. I found no path by which the hub secret, a user id or a payload reaches stderr, logs or errors.

## 6. Verdict

**SHIP.** All automatable criteria pass in my independent end-to-end tests against the real actor and scheduler, and all three fixer claims are confirmed by the code and by mutations. fmt, clippy and 7 test runs are clean. What remains is the manual live check (criterion 6 plus removal of the buttons by the empty keyboard), which belongs to the orchestrator after merge.

## Cleanup

No services were started. `%TEMP%\cctg-t014-qa-target` and `scratch/qa/ws` were deleted. The state directories `%TEMP%\cctg-qa-*` are removed by the tests themselves (`Drop`). `scratch/qa/qa_relay.rs` is kept as evidence.

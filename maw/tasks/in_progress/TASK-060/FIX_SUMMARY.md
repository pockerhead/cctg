# TASK-060 fix summary (fixer)

Branch `fix/flaky-tests` rebased onto local `main` (c4d4cea) first; clean.

Preflight claim checked before acting: review finding 2 suggests "when `reply_to` matches no known message, fall back to the single open in-flight ask of that thread". Taken verbatim, a reply to any other message (a subagent block, an older answer) sent in that window would be eaten as the question's answer. `Asks::text_target` (questions.rs) and the TASK-038 review repro test say exactly that such a reply "answers nothing". So the claim is real as a bug, but its suggested fix is wrong; I used a hold instead (see 2).

## Fixed

1. **Permission press before the Delivery ("Запрос устарел" while the prompt stays open).**
   - Reproduced first: `permission_hook_e2e` Fake got `PROMPT_ANSWER_LAG` (300 ms on `permission: true` sends, like `QUESTION_ANSWER_LAG`). Before the fix `a_press_in_the_topic_is_the_hooks_decision` FAILED after 90 s (hook gave no decision, `EOF while parsing`). After the fix 5/5 pass in about 4 s.
   - `Prompt.thread_id` (the topic it was handed to, set in `send_prompts`) and `Prompts::in_flight(request_id, thread_id)`: the single prompt with that id that is `sent`, has no `message_id` yet and is in the pressed message's topic. `None` if ambiguous or the topic is unknown. `Slots::press` uses it when `by_message` misses. A later Delivery sets `message_id`, and an already decided prompt gets its final edit (`Prompts::delivered` sets `Edit::Due` for a non-active prompt).
   - Side effect: `Prompt` grew past clippy's `large_enum_variant` limit, so `Opened::Added.expired` is now `Option<Box<Prompt>>` (two `*gone` call sites).
   - Unit test `a_press_before_the_message_id_finds_the_prompt_in_flight`.
2. **A reply to the question message before its Delivery.** The hub cannot know that `reply_to` is the question's message until the Delivery comes. So `Slots::hold` keeps back an explicit text reply whose `reply_to` matches no ask, but only while the topic has an open question still sending (`Asks::sending_in`). Later messages of that topic wait behind it, so order is kept (cap `MAX_HELD` = 64, beyond that they go on at once). `release_held` runs after every question Delivery (`Done::Question`) and on hub stop, and pushes the held messages through `on_topic_message` again. A reply to the question message then answers it, and any other reply goes on to the session as before.
   - Test: `own_text_answers_after_other_or_as_a_reply` no longer waits for the edit. It sends a reply to 777 and a reply to the question inside the 300 ms lag. It asserts the answers are `Pear, and a fig` and that the agent gets "not for the question" (and not "fig"). With `hold` disabled the test failed (hook waited 60 s). With it, the test passes.
3. **`own_tmp`.** A `OnceLock`: the first call per process removes and recreates `<CARGO_TARGET_TMPDIR>/<own pid>`, so a dead run with a reused pid leaves nothing and the mtime is fresh. A foreign numeric dir is removed only if it is older than 1 h **and** its pid is not a live process (`tasklist /FI "PID eq N" /FO CSV` on Windows, `kill -0` elsewhere). If liveness cannot be told, the dir stays. Output format checked here: a live pid shows `"powershell.exe","25312",...`, a dead one shows `INFO: No tasks...`, and both exit 0.
4. **Thread check in the fallback.** `CallbackInput.thread_id` (from the callback message's `message_thread_id`, only when `is_topic_message`, the same rule as `Inbound`) is now passed in. `Asks::in_flight(id, thread)` and `Prompts::in_flight(id, thread)` require the ask or prompt to be in that topic. An unknown topic means no fallback. The routing unit test pins `thread_id: Some(7)`. Other `CallbackInput` literals got `thread_id: None`, and the question/permission e2e presses got `Some(100)`.

Extra, found by the full run: `reap_e2e::a_start_after_a_killed_session_takes_its_topic` failed once with `NotFound` at `store.save` (reap_e2e.rs:109). Its root was the fixed shared `CARGO_TARGET_TMPDIR/reap-e2e`, and two concurrent runs race on the registry's temp + rename. The review listed it under "Missing coverage" as the next shared-home flake. The root now goes under `common::own_tmp()` (one line). 5/5 on the rerun, 10/10 in the repeat.

## Skipped

- Finding 2's verbatim fix (in-flight fallback in `text_target`): skipped for the reason in the preflight note; replaced by the hold.
- Finding 4's alternative "use in_flight only when no ask ever owned `m`": the thread check the orchestrator chose is enough.
- Finding 5 (spool test lost the absolute SessionEnd budget check): not in the orchestrator's list 1-4. The constants are pinned by `hook.rs` unit tests, as the review says.
- Nits (old `hook-cli-*` etc. dirs at the top of the target tmpdir, IMPL_SUMMARY wording): not in scope. IMPL_SUMMARY's "correct" about the reply wait is superseded by fix 2.
- `free_port` AddrInUse under twin runs (IMPL_SUMMARY §2): not in scope.

## Test results

- `cargo fmt --all --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --workspace --no-fail-fast` (CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target, CARGO_PROFILE_DEV_DEBUG=0): `EXIT 0`, 44 `test result: ok` (`scratch/runs/fixer_full_test.txt`). The first full run had the reap_e2e failure above, and that led to its fix.
- `sh scratch/fixer_repeat.sh` (10 runs per binary, `scratch/runs/fixer_repeat/summary.txt`): permission_hook_e2e 10/10, question_hook_e2e 10/10, status_e2e 10/10, hook_cli 10/10, statusline_cli 10/10, statusline_agent_e2e 10/10, reap_e2e 10/10.
- Before and after: permission press with lag failed before (90 s), passes after; question reply with lag failed with hold disabled (60 s), passes with hold.

# TASK-060 code review (commit 0efaa09, branch fix/flaky-tests)

## Verdict

**SHIP.** The product fix is correct and I proved it is load-bearing. The test changes still catch the regressions they were written for. The fix merges cleanly onto current main (TASK-054/057), and the merged tree is green. What is left are sibling races the implementer already disclosed and two small test-hygiene holes. They belong in a follow-up task and do not block this one.

## Disconfirmation (done first)

Counter-example I chose: **can `in_flight(id)` credit a press to the wrong question?** Concretely: a press on an old question message whose ask was already forgotten, while a new ask with the same 5-letter id is in flight. Here `by_message` misses, and the fallback picks the new ask. The press's `question`/`Option(n)` then answers the new question.

Result: the path exists, but it only triggers on an id collision.
- `callback_data` = `ask:<id>:<question>:<action>` (questions.rs:79). There is no session, topic or message in it.
- The fallback runs whenever `by_message` misses, including when `message_id` is `Some` and points at some other message (slots.rs:4849-4851).
- The ids come from `permissions::hook_request_id()` (permissions.rs:58): random, 25^5 ≈ 9.8M values. `id_taken` only rejects collisions with **open asks of the same session** (questions.rs:515, slots.rs:4634-4640).
- So a wrong credit needs (a) two asks with an equal random id, (b) the new one still in flight (Telegram's answer not back yet, normally ms), and (c) a press on the old message's buttons at that moment. That is about 1e-7 per pair, inside a sub-second window. Two in-flight asks with the same id get `None` (ambiguous) and are safe. Two topics/sessions with different ids never cross.

The counter-example holds only in theory. See finding 4 for the cheap guard.

## Confirmed correct

- **Root cause is real and the fix is load-bearing.** `run()` `select!`s `control.recv()` (callbacks) and `done.recv()` (Deliveries) (slots.rs:1159/1165), so a callback can be handled before its send's Delivery. Before the fix, `press_question` returned `ANSWER_STALE` and the hook waited `question_wait`. Proof: in a %TEMP% copy of the merged tree I removed the `.or_else(|| self.questions.in_flight(id))` line. `question_hook_e2e` then gives 3 passed / 3 failed in 61.00 s (`the_terminal_button_gives_no_decision_at_once`, `choices_in_the_topic_are_the_hooks_answers` and `own_text_answers_after_other_or_as_a_reply` hang to 60 s). With the line restored: 6/6 in 3.01 s.
- **State after an early press is consistent.** `sending` stays true until `on_question_done`, and `finished()` requires `!sending` (questions.rs:425-430). So `check_questions` can hand the answers to the hook, but it cannot forget the ask before the Delivery. The Delivery then sets `message_id` and `shown = <send version>` (slots.rs:4789-4791). The press already bumped `version`, so `edit_due()` holds and the final edit goes out. If the send failed, `end_question(Expired)` is a no-op on an ask that already ended (`Ask::end` returns false). The hook got its answer, and no double decision happens.
- **Ambiguity is handled.** `in_flight` returns `None` when two in-flight asks share an id (questions.rs:479-486). There is a unit test for it: `a_press_before_the_message_id_finds_the_ask_in_flight`.
- **The `ask.id == id` filter** after the lookup (slots.rs:4858) still guards the `by_message` path against a foreign id.
- **Subagent test.** The extra op is the TASK-033 "закончил" reply, and the test now expects exactly 3 replies with `reply_to` {1000,1001,1002}. The 1.5 s quiet window is longer than `min_gap`, so a 4th reply would still show. On the merged tree (main's TASK-054 reworked the scheduler: edits metered per group) it passed **10/10**.
- **spool_e2e rewrite.** The sentinel arithmetic is right. The first call returns baseline connections. The second returns baseline + 1 (the first sentinel) + the main hook's connections. `- (before + 1)` isolates the main hook. Both hooks have exited before their sentinel connects, so FIFO accept puts them ahead of it.
- **Merge onto main.** `git merge-tree --write-tree main HEAD` is clean (tree 79f572f). None of main's 8 changed files touches `press_question`, `questions.rs` or the changed tests. On the merged tree `cargo clippy -j 1 --workspace --all-targets -D warnings` is clean, and `cargo test -j 1 --workspace --no-fail-fast` passes all 44 binaries.
- No new crates. No secrets or ids in tests.

## Issues

### 1. major (follow-up, disclosed): permission prompts lose a press made before the Delivery, and the answer says "Запрос устарел"
`slots.rs` `press()` → `self.prompts.by_message(message_id)` (≈slots.rs:5053-5056). `by_message` fills only when the send's Delivery arrives. A press that wins the same `select!` race gets `ANSWER_EXPIRED` ("Запрос устарел"), but the prompt stays **open**. The user reads "the request is outdated" and will not press again. A hook prompt (`prompt.hook`) then waits out its timeout, and a channel prompt stays waiting in Telegram until the terminal answers. That is the same user-visible loss that TASK-060 fixed for questions, plus a misleading toast. `permission_hook_e2e.rs:351` presses as soon as the fake has recorded the send, so it is exposed to the same flake. Fix: the same fallback, i.e. a single prompt with `request_id == id` that was handed to Telegram and has no `message_id` yet. Callback data `allow:<id>`/`deny:<id>` carries the id (permissions.rs:70). Add a lagging-fake test like `QUESTION_ANSWER_LAG`. Out of this task's listed scope, but it should get a task.

### 2. minor: a reply to the question message before the Delivery is not taken as the answer; the test was changed to wait instead
`question_hook_e2e.rs:431-436` now waits for `last_edit_of(message_id)` before `say(..., Some(message_id))`. The justification ("a real user cannot reply to a message before it exists") does not hold. The message exists in Telegram, and the user can see it and reply to it, before the hub's Delivery sets `ask.message_id`. `Asks::text_target` matches a reply only by `ask.message_id == Some(reply_to)` (questions.rs:493-498). So a reply in that window does not answer the question: it goes to the session as ordinary inbound and the question stays open. The window is the same as in issue 1. Typing a reply takes seconds, so it is much less likely than a button press. Suggested fix: in `text_target`, when `reply_to` matches no known message, fall back to the single open in-flight ask of that `thread_id`. Or at least state the limit in the test comment instead of calling the wait "correct".

### 3. minor: `own_tmp()` keeps a stale dir on PID reuse and can delete a live run's dir in the same case
`tests/common/mod.rs:56-76`. The own dir is never cleaned (`name != own`). If Windows reuses the pid of a crashed run from within the hour, the new run inherits its leftover `.cctg/spool`. `hook_cli::home()` only does `create_dir_all` and rewrites `device.env`, but does not clear the spool. That is the "spool replayed into the next run" failure that this change was meant to remove. In the same case the reused dir keeps its old mtime: directory mtime changes only when direct children are added, and the `<prefix>-<test>` subdirs already exist. A concurrent run's cleanup can then `remove_dir_all` a live run's home. Both need a pid reused within an hour, so the probability is low. Fix: remove the own dir once per process (for example with a `OnceLock`) before returning it, and touch it so its mtime is fresh.

### 4. minor (hardening): the fallback ignores which message was pressed
`slots.rs:4849-4851`. When `message_id` is `Some(m)` and `m` is a known message of a *different*, finished ask (or of any other hub message), the fallback can still pick an in-flight ask with an equal id (see the disconfirmation above: about 1e-7). A cheap guard is to use `in_flight` only when no ask in the book has ever owned `m`, or to carry `thread_id` in `CallbackInput` and require `ask.thread_id == Some(thread)`. Not required for SHIP.

### 5. minor: the spool test's time bound no longer checks the absolute SessionEnd budget
`spool_e2e.rs` now asserts `took < baseline + 2 × POST_TIMEOUT`. The regression it targets (a timeout per kept file, about +7.5 s) is still caught. A replay plus own-event that costs two budgets is still caught by `connections <= 1`, because it makes a second connection. What the test lost is the absolute check "the process stays inside the 1.5 s SessionEnd budget". If `POST_TIMEOUT` itself grew, both runs would grow together. `hook.rs` unit tests pin the constants (`post_timeout` tests around hook.rs:2184-2212), so this is covered elsewhere. I note it only because the doc comment dropped that sentence without pointing to where the constant is guarded. `<= 1` also accepts 0 connections: a hook that silently skipped the replay would pass this test, but other spool tests (`a_missed_session_start_reaches_the_hub_before_the_next_hook`) cover replay against a live hub.

## Other buttons checked for the same race (as the orchestrator asked)

| Button | Lookup | Can a real press be lost before the Delivery? |
|---|---|---|
| Question (`ask:`) | by_message, now + `in_flight(id)` | No longer (fixed, proven by the revert) |
| Permission (`allow:`/`deny:`) | `prompts.by_message` only | **Yes**, with the answer "Запрос устарел" (issue 1) |
| Question reply text | `text_target` by `message_id` | Yes, the reply goes to the session instead (issue 2) |
| Status ⏹ / status buttons | `status_slot(message_id)` (slots.rs:2693) | Only a press on a status message just created, before its first Delivery. It gets `ANSWER_STALE`, a second press works, and ⏹ needs two presses anyway. Negligible |
| Resume (`resume:<session>`) | session id in the data (`press_resume`, slots.rs:3578) | No, no message lookup |
| Update (`status::parse_update` → session) | session id in the data | No |
| `/devices` roster (`dev:*`) | device id in the data (roster.rs:118) | No |

## Missing coverage

- A lagging-fake test for permission prompts, the same pattern as `QUESTION_ANSWER_LAG` (issue 1).
- A question test where the reply-to-message arrives before the Delivery (issue 2). It documents the behaviour either way.
- `own_tmp` reuse: a unit test that plants a stale `<own pid>/…/.cctg/spool` and checks it is gone (issue 3).
- Other tests still keep fixed homes under the shared `CARGO_TARGET_TMPDIR`: `reap_e2e.rs:42` (`reap-e2e/.cctg`), `agent_stdio.rs:70`, `isolation.rs:101`, `stdout.rs:7/31/51`. None of them writes a `device.env` with a port, so they are not the diagnosed collision. `reap_e2e` does keep `.cctg` state there and may be the next concurrent-run flake.

## Nits

- `own_tmp` leaves the old `hook-cli-*`, `statusline-cli-*` and similar dirs at the top of `CARGO_TARGET_TMPDIR` forever. Once is fine.
- IMPL_SUMMARY §1 calls the reply-wait "correct". It is a test accommodation of issue 2.

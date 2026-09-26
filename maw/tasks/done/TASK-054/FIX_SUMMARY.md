# TASK-054 FIX_SUMMARY (fixer)

The fix is on top of `1ccecd6` on `feature/outbound-debounce` (commit `010b9c5`).
Files: `hub/scheduler.rs`, `hub/slots.rs`, `hub/status.rs`, `hub/roster.rs` (one field), `tests/status_e2e.rs` (one pattern).

Preflight, the claim that would have broken things if applied as written: review issue 5 says to make `due()` `break` at a permission prompt of the same topic. Together with orchestrator decision 4 that is wrong. `next_message` skips the prompt because its topic was already seen through the waiting line, so when the message bucket is empty `pick` sleeps until the line's debounce and the prompt waits too. I checked this against `pick`/`next_message` and pinned it with a test. What I did instead is below (item 5).

## 1. Fixed

1. **[critical] Topic queue starves (issue 1, decision 1).** `next_edit` now returns a `Lane` and serves the edit budget in this order: callback answers (no token), then topic calls and foreground edits taking turns (`topic_turn`), then background edits. My first attempt put Topic strictly before all edits. It failed the new slots test: the permission decision waited 14.9 s behind a pin and two icon edits. I switched to strict alternation (the orchestrator's second option), which is recorded in `log.jsonl` as a dead_end. Under continuous load a topic call waits at most one token (4 s) when it is alone, and at most two when foreground edits also wait. Tests:
   - `topic_calls_and_foreground_edits_never_wait_behind_status_refreshes`: 10 slots refresh forever. Over 3 minutes, CreateTopic, EditTopic, Pin, Delete, an edit and a reaction each go in ≤ 4 s. The edit budget is ≤ 20 in any 60 s.
   - `topic_calls_and_foreground_edits_take_turns_ahead_of_refreshes`.
   - A mutation check with the old ordering: the tests fail, they do not hang (`waited` has a 60 s timeout).
2. **[major] Status edits hold up user-facing edits; ⏹ does not work (issue 2, decision 2).**
   - Added `Op::Edit::background`. Only `pump_status` refreshes set it; every other edit (decisions, questions, blocks, Resume, roster) passes `false`, and reactions are always foreground.
   - Coalescing makes the queued entry foreground if either the old or the new edit is foreground. Background edits are FIFO with one queued edit per message, which amounts to round-robin by slot.
   - On a 429, an edit or reaction whose message got a newer queued version in the meantime is answered `Superseded` and not put back. Without this, the old text could overwrite the new one.
   - In slots, `Shown.busy` is now `in_flight: u8` plus `urgent`. A ⏹ press (arm, confirm, disarm while waiting, Esc written) sets `urgent`. That edit goes as foreground, even while a refresh of the same message is still queued; the scheduler puts it in that refresh's place, and the `Superseded` answer does not change `content`.
   - `CONFIRM_FOR` now restarts when Telegram accepts the edit that first shows the "Точно прервать?" question (`status::asks_confirm`).
   - Tests:
     - `every_status_refresh_goes_within_a_bound_and_the_slots_share_the_rest`: every slot gets refreshed, each wait ≤ 90 s, counts differ by ≤ 1.
     - `a_foreground_edit_takes_over_a_queued_refresh_of_its_message`.
     - `a_refresh_refused_with_429_gives_way_to_the_newer_edit_of_its_message`.
     - `a_stop_question_replaces_a_waiting_refresh_and_gets_its_whole_wait_once_shown`: a second press 14 s after the first, 5 s after the question showed, still interrupts.
3. **[major] The real hub path is not covered (issue 3, decision 3).** New test `slots::tests::with_the_hubs_pacing_topics_prompts_and_stop_stay_quick_under_status_churn`, using `Limits::default()` and paused time. Setup: 11 live sessions with keys agents and pinned status, and 10 of them change status every 2 s. Checked:
   - a new session's topic ≤ 10 s;
   - the permission prompt ≤ 5 s;
   - the Allow decision edit ≤ 8 s;
   - the ⏹ question ≤ 8 s, and the second press gives `ANSWER_INTERRUPTING` plus a `ConsoleKey` to the agent;
   - over the next 150 s every churning slot gets ≥ 3 status edits, with gaps ≤ 90 s.

   Helper `rig_with(.., limits)`; `rig_in` delegates to it.
4. **[minor] A prompt overtakes the lines of its own topic (issue 4, decision 4).** With a debounce, `next_permission` returns the first stream line of the prompt's topic when that line is `merge`. It goes at once, together with the lines that join it, and the prompt goes next. An answer (`merge: false`) is still overtaken, as before. A prompt of another topic still goes first. When there is no debounce (fast `BucketConfig` tests) nothing changed. Tests:
   - `a_permission_prompt_does_not_wait_for_the_debounce` became `a_permission_prompt_lets_the_held_lines_of_its_topic_go_first_at_once`: a+b at 100 ms, prompt at 1.1 s, c at 2.1 s.
   - `a_prompt_of_another_topic_still_goes_before_waiting_lines`.
5. **[minor] `due()` waits for lines after the prompt (issue 5, decision 5).** `due()` now stops at a same-topic prompt and returns at once, which is stronger than `break`. Lines after the prompt never extend the wait, and lines before it go without the debounce. Test: `lines_held_before_a_prompt_go_when_the_budget_allows_not_after_the_debounce`. The line goes at 1.0 s, when the gap allows; a literal `break` would give 1.6 s.
6. **Nits.**
   - The `EDIT_BUCKET` doc now says N × 4 s for refreshes and 4-8 s for topic calls and foreground edits.
   - The module docs of the scheduler and slots describe the new order and the edit classes.
   - A PCTX addendum is in `PCTX_PROPOSALS.md`.

## 2. Skipped

- **Issue 6 (live soak with `Limits::default()`).** Telegram, the server and `.env` are off limits for this stage. The 20/min edit budget is still a guess until a live check.
- **Issue 2, alternative "`STATUS_EVERY` grows with the number of slots".** Not needed: the background class and round-robin already bound the refreshes.
- **"`CONFIRM_FOR` from Telegram acceptance instead of a priority edit".** I did both: the edit has priority, and the window restarts when the question shows.

## 3. Test results

All builds used `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`. Before the run I touched `lib.rs`, `main.rs` and `tests/status_e2e.rs`.

- `cargo fmt --all -- --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --workspace --no-fail-fast`: EXIT=0 across 44 test binaries, 946 passed, 0 failed, 3 ignored. The lib alone: 746 passed, 1 ignored. Log: `scratch/fixer-test-full.log`. There were no flakes, so nothing needed rerunning.
- The new timing tests (`hub::scheduler`, `with_the_hubs_pacing`, `a_stop_question`) passed 6 times in a row: 47/47 each time.

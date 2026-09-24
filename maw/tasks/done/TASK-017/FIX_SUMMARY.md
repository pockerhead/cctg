# TASK-017 FIX_SUMMARY (fixer, claude opus, medium)

Preflight claim checked first: the literal fix for Minor 2 ("drop the old button with `drop_resume_button` and offer a new one") would edit the old message to `RESUMED_TEXT` ("снова на связи, сохранённые сообщения доставлены"), which is false in that case because nothing was delivered. I did not use that fix. See Minor 2 below.

## 1. Fixed

- **Minor 1 (a late answer from an earlier period takes the new note).** Confirmed by reading `on_resume_done`: the filter was `note.session == session && message_id.is_none()`, and the same session A matches in both periods. Fix: `ResumeNote.number: u64` (`#[serde(default)]`, older files load as 0), plus a per-run counter `Slots.resume_sends`. `Work::Resume`/`Done::Resume` now carry `number` in place of `session`, and the note is matched by `number`. A persisted note from an earlier run can't collide, because a Done from an earlier process never arrives and a new note is only created when the slot has none. Test `a_late_answer_of_an_earlier_period_never_takes_the_new_button` covers it: two sends in flight, the first answer edits its own message to `RESUMED_TEXT`, and the second message keeps its button with id 902. Mutant check: with the number filter disabled, the test fails.
- **Minor 2 (the Resume button of an older session in a slot where a later channel-less session ended).** Confirmed: `press_resume` only accepted the slot's current session, so the button for A answered `ANSWER_EXPIRED`. Fix (the "the press must act" option): a press is also accepted for a Dead slot whose open note names the pressed session. That records `resume_asked`, and the target is the session the button names, A (this is what TASK-019 reads, per OPEN_DECISIONS Q3). I rejected sending a new button for the latest ended session: it adds a message per headless run, needs a new edit text, and the latest session might be a maw `claude -p` run. Recorded as a decision in log.jsonl. Test `a_resume_press_still_counts_after_a_later_session_ended_in_the_slot`. Mutant check: with the note path turned off, the test fails.
- **Minor 3 (flaky timing tests).** `the_agent_calls_of_an_ended_session_are_forgotten`: `drain_done` (200 ms of silence) is replaced by `drain_until(slots, done, ready)`, which waits on `done.recv()` until the condition holds, with a `WAIT` timeout. `one_slot_lives_through_hook_agent_end_and_the_next_session`: the `sleep(200ms)` is replaced by polling `registry.json` until `slots[0].current_session == B`, with a `WAIT` timeout. Under load, 8 rounds of 4 parallel runs of the lib test binary with filter `hub::` gave 0 failures out of 32. The reviewer saw 9/36 failures on the branch.
- **Nit (full id in the Resume text).** `resume_text` now shows only the short id in the first line. The terminal hint became "claude --resume и выберите эту сессию." (`claude --help`: `--resume [value]` takes a session ID or opens the picker; I did not check whether a short prefix works as a search term, so it is not offered as an argument). The test checks that the full id is absent. The trade-off: the ready-to-paste `claude --resume <uuid>` command is gone.
- **Missing coverage (`on_resume_done` after the period closed).** Added `a_resume_message_answered_after_its_period_ended_loses_its_button`: the session revives before Telegram answers, the buffer becomes idle, and a late answer produces one edit with `RESUMED_TEXT`. The race test above covers the variant with an open next period.

Test harness: `capture_dispatch` swaps `Slots.dispatch` for a channel the test reads. Telegram answers only the `Done` that the test feeds itself, so the order of answers is fully deterministic.

## 2. Skipped

- Missing coverage "overflow when `send_messages` was refused by the limit" and "the agent disconnected between two `flush` calls": not in the fix scope from the orchestrator. The code paths are plain (`overflow_told` / `queued_told` are only set when the send was accepted). Left open.
- Nit on the `QUEUED_NOTICE` wording ("дойдут, когда она подключится"): not in scope. The behavior matches OPEN_DECISIONS.

## 3. Test results

One `CARGO_TARGET_DIR=%TEMP%/cctg-t017-fix`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, `--offline`. The directory was deleted at the end.
- `cargo fmt --all --check`: ok.
- `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --offline --workspace --no-fail-fast`: all ok. cctg lib 393 passed / 1 ignored (was 390, +3 new tests), buffer_e2e 1, stream_e2e 11, hook_cli 7, the rest ok (`scratch/fixer/workspace_test.txt`).
- Load: `<lib test exe> hub::`, 4 in parallel × 8 rounds: 0/32 failures.

Changed files: `crates/cctg/src/hub/buffer.rs`, `crates/cctg/src/hub/slots.rs`, `crates/cctg/src/hub/registry.rs` (one line in a test). Nothing committed.

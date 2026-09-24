# FIX_SUMMARY — TASK-029

## Step 0: merge of `main`
`git merge main` (TASK-039 reaper, 3203591) applied with no conflicts and was committed as `5ba15e6`. The merged tree compiled, and so did all test targets. The full workspace test ran once at the end, with the fixes on top (see below). The fixes are in `c6a7245`.

## Pre-check: the review claim that would break correct code
The claim: M2's suggested fix "a `ToolEnd` / stream `Result` for the session means its blocking prompt was answered: quiet that session's prompts".

Checked against the code:
- `PostToolUse` is registered `async` (docs/hook-settings.json). The end of call N reaches the hub about 0.1 s after the call. The next call's `permission_request` travels over the already open agent link, which is faster.
- Stream lines can lag by any amount: Telegram pacing, rewinds, `STREAM_QUEUE`.

So a verbatim fix would quiet a prompt that is still open. It would show ⏹ and allow an Esc that answers the prompt. The claim is real as a risk, so the fix below narrows it.

## Fixed
- **M2. A prompt answered in the terminal left the status on "waiting".**
  - `Prompt.opened` is new, and so is `Prompts::quiet_settled(session, now, settle)`.
  - A `ToolStart`/`ToolEnd` hook of the same live top-level session quiets that session's prompts that came in at least `Options::prompt_settle` ago (`PROMPT_SETTLE` = 2 s). It then calls `sync_waiting`, so both the icon and the ⏹ gate follow.
  - Younger events and stream lines never quiet a prompt.
  - The interrupt note in the stream now also quiets the session's prompts and calls `sync_waiting`, as Stop does. This is what plan-reviewer-2 described. It acts only when `interrupted_at` reports a new note, so a note read again after a rewind does nothing.
  - Buttons of the quieted prompts stay.
  - Tests:
    - `slots::tests::a_prompt_answered_in_the_terminal_stops_waiting_once_a_later_call_comes`: a late ToolEnd keeps it waiting; a settled later ToolStart gives ⏹ back and turns the icon off.
    - `slots::tests::an_interrupt_note_at_an_open_prompt_ends_the_wait`: ❓ goes to 💤.
    - `permissions::tests::a_later_call_quiets_only_prompts_older_than_the_settle_time`.
- **m1. A late ToolStart after Stop reopened the turn.** `Activity::tool_start` no longer sets `turn`. `running` alone drives the ⚙️ phase and `busy`. Test: `status::tests::a_start_after_stop_does_not_open_the_turn_again`.
- **m2. "Esc отправлен" appeared only after the pace.** `on_key_written(written = true)` sets `shown.next_at = now` for the slot. Test: `slots::tests::a_written_esc_is_shown_at_once_whatever_the_edit_pace`.
- **m3. Output over 64 KiB made the status line wait 10 s.** `run_chained` reads up to `MAX_OUTPUT` and then drains the rest into `tokio::io::sink()`, so the command never blocks on a full pipe. The exit code is kept. On timeout the child is killed by `kill_on_drop`; this is now stated in a comment. Test: `statusline_cli::a_command_that_prints_too_much_is_cut_without_waiting_for_the_timeout`. The command prints 300 000 bytes; the output is exactly 64 KiB, the exit code is 0, and the run takes under 5 s. Before the fix it took 10.7 s and fell back to cctg's line.
- **m4. `Activity` was not cleared when the TASK-039 reaper ended a session.** After the merge, reaped ids are already part of `followup.ended_sessions`, so the old removal loop covered them. The loop is replaced by `activity.retain(is_live_top_level)` after every hook. This also covers the end by a reused pid, which is not in `ended_sessions` (registry.rs:802). Test: `slots::tests::a_session_whose_process_died_ends_as_by_its_session_end` now checks that the activity is dropped.
- **m5. A failing status Create warned on every retry.** `Shown.send_warned` makes this one warn per episode. It resets on a successful create; the repeats go to debug. Test: new binary `tests/status_logs.rs`, `a_status_message_that_keeps_failing_is_warned_about_once`: 1 WARN for 3 or more tries.
- **Process: IMPL_SUMMARY.md was missing.** It is written now.

Every test above was checked for fail-before by reverting the fix (`scratch/fixer/mutations.out.txt`):
- M2, no quiet: FAILED.
- M2, quiet without settle: FAILED.
- Interrupt quiet removed: FAILED.
- m2 pace reset removed: FAILED.
- m3 drain removed: FAILED.
- m5 warn every retry: FAILED.

m4 is not fail-before, because the merge already covered the reaper path.

## Skipped
- **M1 (a channel-started turn never sets `turn`): not real.** The orchestrator's live hub log (`~/.cctg/supervise.log`) shows every "message forwarded to the session agent" followed 60-100 ms later by `user_prompt_submit` for the same session (for example 16:30:23.689 -> 16:30:23.753). `UserPromptSubmit` does fire for channel messages. The task forbids touching `~/.cctg`, so I did not reread it.
- **m2 option "hide ⏹ while a KeyAsk is outstanding":** not needed for the scope item, which is to show "Esc отправлен" at once. The review's aside is inaccurate: until `ConsoleKeyWritten` arrives, `busy` is still true, so a press in that window arms a new confirmation. It does not answer `ANSWER_IDLE`. The window is one agent round trip (milliseconds), and a second Esc still needs a second confirming press, so I left it.
- **Nits** (process-tree walk for `sdk-cli` agents; `ANSWER_OFFLINE` text when the agent queue is full): outside the fix scope and harmless.

## Known limit (new, also in PCTX_PROPOSALS.md)
A parallel call (concurrent Read or Agent calls) of the same session that starts or ends 2 s or more after a sibling's prompt came in quiets that prompt too early. ⏹ would then offer an Esc that answers the prompt. The permission request carries no `tool_use_id`, so the hub cannot tie a hook event to the prompted call.

## Test results
Using `CARGO_TARGET_DIR=%TEMP%/cctg-fix029-tgt`, `CARGO_PROFILE_DEV_DEBUG=0` and `-j 1`. The target dir was deleted afterwards.
- `cargo fmt --all --check`: exit 0.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0 (`scratch/fixer/clippy.out.txt`).
- `cargo test -j 1 --workspace`: exit 0. **632 passed, 0 failed, 2 ignored** (`scratch/fixer/workspace_test.txt`). The lib had 478 passed and 1 ignored. The 614 of the review plus the TASK-039 tests plus 7 new tests make up the total.

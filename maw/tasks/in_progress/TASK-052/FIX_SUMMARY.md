# TASK-052 fix summary

## CI run 1 fix

Input: CI run 1 on `fix/install-path-channel-off` (commit d5e38de). Windows and image were green. There were three failures (`scratch/ci/run1_failed.log`). This run had no review file. The orchestrator's list of failures was the input.

Preflight claim checked first: "the ubuntu console failure comes from TASK-052's channel watch". Acting on it (for example by removing or gating the watch) would have broken a correct feature. It is false. `status_e2e` builds its `Options` from `Options::default()`, where `channel_wait` is `ZERO`. With that value `expect_taken` returns before it inserts, so `unseen` stays empty and `check_channel` returns on its first line. Everything else d5e38de added to `slots.rs` only removes entries from those empty collections. d5e38de changes nothing on the console-command path or on the permission-hook path in these tests.

### Fixed

1. **macOS: `install_e2e::a_container_gets_its_host_name_written` (install_e2e.rs:683)**
   - Cause: the test fakes a container with env `container=docker`. On macOS `in_container` said yes, so `choose_host` printed "warning: in a container ..." and wrote nothing. `write_device_env` then took its macOS branch and wrote the Mac's `LocalHostName`. So the script warned about a container and then used the Mac's name anyway.
   - Decision: a Mac is never a container. Docker on macOS runs Linux, and a macOS machine has its own stable name, which the macOS branch writes.
   - `install.sh`: `in_container` returns false when `os=macos`.
   - Test: on macOS the first run checks that there is no container warning and that the macOS-marked `CCTG_HOST` pair is written. The other systems keep the existing warning, env `CCTG_HOST` and "stays once written" steps. The `--host` part runs everywhere. The user's extra `CCTG_HOST=mine` line is now appended at the end, not placed after `dev-box`, so the macOS path can use it too.
   - shellcheck 0.11.0 with `--shell=sh`, `dash` and `bash` reports nothing. `install.sh` is still LF.

2. **ubuntu: `status_e2e::a_console_command_goes_over_the_link_and_its_answer_comes_back` (status_e2e.rs:920)**
   - Cause: a race in the test, not caused by d5e38de. The sequence is:
     1. `serve_agents` hands `AgentEvent::Registered` to the actor's agents channel.
     2. It then answers `registered` to the agent, so `Agent::connect` returns.
     3. The test sends `Control::Message("!echo hi")` on the control channel, which is a separate channel.

     `Slots::run` uses an unbiased `select!`. If the actor has not woken up between steps 1 and 3, both messages are ready and the control message can win. `on_console_command` then finds no `live_agent` and replies with `OFFLINE_NOTICE`. The agent gets nothing, so the test sees `None` after 30 s.
   - Proof: `scratch/fixer/register_race_probe.diff.txt` is a temporary patch that answers `registered` first and hands the event over 300 ms later. With it the old test failed exactly as in CI: `status_e2e.rs:920:22`, "no console command: None", 30.03 s. With the fix all 7 status_e2e tests passed under the probe. The probe also broke `a_late_key_answer_never_reaches_the_next_session_of_the_slot` ("no console key: None"), which has the same race on the key path, so it got the same wait. The probe was then reverted.
   - Test fix: new `Hub::agent_bound(thread)`. It waits until the topic shows the alive icon (`topic_icon` reads the icon from creation and from later icon edits). The registry derives that icon from a bound agent. The panic at line 920 now also prints the ops, so a refusal notice would be visible.
   - Why not change the product: no real agent is exposed to this race. Console commands come from Telegram, and the window between the hand-over and the bind is microseconds.

3. **macOS: `permission_hook_e2e::a_session_end_answers_the_waiting_hook_without_a_decision` (permission_hook_e2e.rs:132, "hook prompt in time")**
   - Not caused by d5e38de: the same inert `channel_wait` argument applies. Before the prompt, this test runs the same code as `a_press_in_the_topic_is_the_hooks_decision`, which passed twice in the same run, and as `a_hub_that_stops_...`.
   - Cause: a flaky test. The prompt list stays empty for 30 s only when the hook process ended before its ask became a prompt. Its connect and send have `PERMISSION_CONNECT_TIMEOUT` = 500 ms. The test never looked at the hook, so it waited the full 30 s with no diagnosis. The CI log cannot show which early exit happened, because the hook's stderr was not printed.
   - Proof: `scratch/fixer/hook_connect_probe.diff.txt` adds a temporary 600 ms stall inside the hook's send future. With it the old test failed exactly as in CI: `permission_hook_e2e.rs:132:29`, "hook prompt in time", 30.04 s.
   - Test fix: new `hook_with_prompt`, used by the 3 tests with this pattern. It waits for whichever comes first, the prompt or the end of the hook process. A hook that ends first is started again, at most 3 times, and each run's exit status, duration and stderr go into the panic.
   - Checked with the probe in two forms:
     - The stall on every run: the test fails in 2.2 s and shows three `hub did not answer within 500ms` lines.
     - The stall only on the first run of each home, using a marker file: all 5 tests pass, and the markers show the first runs did stall.
   - The probe was then reverted. The product constant is unchanged: it keeps a stopped hub cheap for Claude Code on Windows.

### Skipped

- Nothing from the three failures.
- Side finding, not fixed in the code: the first full-workspace run failed `transcript/tests/purity.rs::every_source_file_is_scanned` with NotFound.
  - Cause: the workspace-profile binary `purity-f83d3eae107f354c.exe` was built at 14:05 by the TASK-038 planner's reference workspace `C:\Users\user\AppData\Local\Temp\cctg-task038-ref` (since removed) into the shared target. It has that copy's `CARGO_MANIFEST_DIR` baked in. Fingerprints of workspace members are path-relative, so cargo reused the binary here.
  - Remedy applied: `touch crates/transcript/tests/purity.rs` (mtime only) forced a rebuild from this tree, and the test passed.
  - Recorded in `PCTX_PROPOSALS.md`, together with the registered-before-bound lesson.

### Test results

Environment: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`.

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 -p cctg --test status_e2e --test permission_hook_e2e --test install_e2e`:
  - install_e2e: 10 passed.
  - permission_hook_e2e: 5 passed.
  - status_e2e: 7 passed.
- `cargo test -j 1 --workspace --no-fail-fast` (`scratch/fixer/workspace_test.txt`): 41 binaries, 840 passed, 1 failed (the stale purity binary above), 3 ignored. After the touch, `cargo test -j 1 --workspace --test purity` gave 3 passed. Total: 841 passed, 0 failed, 3 ignored, the same count as the implementer's run.
- Not run here:
  - the macOS branch of the container test;
  - the Linux runner.

  Windows covers the non-macOS branch. The macOS branch is covered by reasoning: `os=macos`, so `in_container` is false, `host_line` is empty, and the macOS branch writes the pair the CI log already showed. The next CI run checks both.
- The unix cross check was not needed: no unix-only Rust code changed.

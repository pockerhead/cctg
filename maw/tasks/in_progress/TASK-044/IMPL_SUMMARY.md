# TASK-044 IMPL_SUMMARY

Branch `feature/linux-console`, code commit `129b803` on top of `be39521` (main 239dbcf + maw-only commits; `git diff 239dbcf be39521 -- . ':!maw'` is empty).

## Pre-flight

The plan's reference is a byte-exact patch. `git apply --check --ignore-whitespace scratch/reviewer2/task044.patch` was clean. After applying, `verify_hashes.sh` gave 16/16 OK, so every file, function and signature the plan names exists in the shape it describes. Spot checks against code: `update::request_path` = `<state>/restart/<pid>.json` (the e2e relies on it), `main.rs:174` gives `cctg run` the same `DeviceConfig.state_dir` the agent uses (the socket path matches on both sides), and `install.sh:576` starts the wrapper with `exec` (so `cctg run` is the shell's job process and its self-SIGSTOP shows up as a stopped job).

## 1. What was implemented

The reference patch, as is (PLAN_FINAL steps 1-10). `git diff --stat be39521 129b803`:

| file | change |
|---|---|
| `Cargo.lock` | +34 (vt100 0.16.2, vte 0.15.0, arrayvec, unicode-width) |
| `crates/cctg/Cargo.toml` | +15/-5 (vt100; unix `libc` as a normal dependency; `[[test]] run_pty_e2e`) |
| `crates/cctg/src/term.rs` | new, 639 lines (Ask/Screen/answer/clients; unix `Host`: pty, raw relay, socket, wait/Ctrl+Z, hang_up) |
| `crates/cctg/src/keys.rs` | +371/-121 (`Target { Console, Run }`, `Terminal` trait, shared `watch`/`close_panel`/`cancel_exit_dialog`, `Run` writer, Windows `Attached`, fake-terminal tests) |
| `crates/cctg/src/run.rs` | +156/-23 (`Place { Console, Pty }`, `follow_signals`, stop without restart) |
| `crates/cctg/src/proctree.rs` | +188/-3 (macOS `proc_pidinfo`/`proc_pidpath`/`proc_listallpids` walk, `process_name`) |
| `crates/cctg/src/agent.rs` | +41/-11 (`console_target`: Windows console, otherwise the socket of the `launched_by` cctg run when it answers) |
| `crates/cctg/src/update.rs` | +21/-13 (`Worker.console: Option<keys::Target>`) |
| `crates/cctg/src/hub/console.rs` | +2/-1 (`NO_CONSOLE_NOTICE`) |
| `crates/cctg/src/lib.rs` | +1 (`pub mod term`) |
| `crates/cctg/src/shim.rs`, `tests/reap_e2e.rs` | cfg now includes macOS |
| `crates/cctg/tests/run_pty_e2e.rs` | new, 551 lines (harness = false; Unix only) |
| `README.md`, `docs/poc.md`, `docs/remote-hub.md` | step 10 texts |

## 2. Deviations

None. I made no code changes on top of the reference. I read `term.rs`, `keys.rs`, `run.rs`, the `agent.rs`/`update.rs`/`proctree.rs` diffs and `run_pty_e2e.rs` looking for defects. I checked the signal and job-control paths (SIGHUP/SIGTERM during start and while stopped, orphaned process groups with SIGSTOP, WUNTRACED reported once, the order raw -> SIGCONT in the e2e), the lock order (master and screen are never held together), the Windows path (`Attached` Drop frees the console before the lock guard goes, same as the old `FreeConsole` at the end), the dead-code/cfg interplay on Unix, and the macOS `proc_listallpids` return unit (a pid count). None of these gave a defect I could prove with a test or the cross check. CLAUDE.md was not touched (orchestrator decision 4).

Leftovers from other stages in the tree (`maw/tasks/in_progress/TASK-048/...`) were left alone and not committed.

## 3. Test results (Windows host, shared target, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`)

- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: exit 0.
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/implementer/test.txt`): 823 passed, 1 failed. `run_e2e: ok`, `run_pty_e2e: Linux and macOS only`. The one failure was `transcript` `purity::every_source_file_is_scanned`: `read_dir(CARGO_MANIFEST_DIR/src)` gave NotFound. `crates/transcript` is not changed by this task and the directory exists, so this was a stale binary in the shared target built from another tree (TASK-048 ran at the same time). After `touch crates/transcript/tests/purity.rs` and `cargo test -p transcript --test purity`: 3 passed. Total 824 passed, 0 failed, which matches the reviewer's count.
- Unix cross check: `scratch/reviewer2/unixcheck/unixcheck.sh C:/Users/user/dev/cctg <scratchpad>/unixmini` with `CARGO_TARGET_DIR` set to the shared target. It runs clippy `-D warnings`, `-Zbuild-std`, on `--lib --test run_pty_e2e` for `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl` and `aarch64-apple-darwin`. All three finished clean (`scratch/implementer/unixcheck.txt`). It does not cover `agent.rs::console_target` or `update.rs` on Unix (platform-neutral code that CI compiles).
- Not run here: `run_pty_e2e` itself, the macOS `proctree` live tests and `reap_e2e` on macOS. These need the Linux/macOS CI runners (no pushes from this stage).

## 4. How to verify manually

1. CI on a pushed branch: `test (ubuntu-latest)` and `test (macos-latest)` print `run_pty_e2e: ok`, and `proctree::tests::the_live_*`, `a_native_claude_on_macos_is_named_claude`, the shim links test and `reap_e2e` pass on macOS. `image` (musl) builds.
2. Live on Linux/WSL and macOS (Terminal, iTerm2, inside tmux) with a `claude-cctg` from this build:
   - the channels dialog is answered by itself and the channel comes up;
   - ⏹ interrupts a turn;
   - `!echo hi`, `/cost` and `/usage` from the topic work (the panel is read and closed);
   - with a draft in the box you get "черновик" and the draft stays;
   - Ctrl+Z / `fg` work, resize works, `/exit` keeps the exit code;
   - "Обновить" restarts claude in the same window with `--resume`;
   - `claude-cctg -p ... | jq` runs without a pty and the topic says the client cannot type commands (`NO_CONSOLE_NOTICE`).
3. macOS: a nested `claude -p` gets no topic, and after `/clear` the agent rebinds.

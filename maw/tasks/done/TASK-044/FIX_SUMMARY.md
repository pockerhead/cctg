# TASK-044 FIX_SUMMARY

## CI run 1 fix

CI run 1 (commit 129b803): windows and image green; ubuntu and macos failed only `tests/run_pty_e2e.rs`. Log: `scratch/ci/run1_failed.log`. Neither Linux nor macOS can be run here, so both causes come from the code and the log.

Before acting: the prompt's candidate list for macOS (`tcsetattr`, `ioctl TIOCSCTTY`/`TIOCSWINSZ`, `openpty` with a zero winsize) does not match the log. `run_pty_e2e.rs:248` is the `tcgetattr` in the harness's `Terminal::raw()` on the test's own slave fd. `TIOCSCTTY` runs in `pre_exec` (a failure there would be a spawn error, not line 248), and `TIOCSWINSZ` is line 269. Changing any of those would not have touched the failure.

### Fixed

1. **ubuntu, `run_pty_e2e.rs:489` "the user's mode while stopped"** (product bug, `crates/cctg/src/term.rs`, `Host::wait`).
   Cause: `Host::wait` runs on a tokio `spawn_blocking` thread (`run.rs:58`), not the main thread. On a stop it did `restore()`, then `kill(getpid(), SIGSTOP)`, then `raw()`. `kill` to the own pid is process-directed. Linux `complete_signal` hands it to the main thread first when that thread wants it (the tokio main thread is parked, so it does). The waiting thread comes back from `kill` without a pending signal and gets to `raw()` (`tcsetattr`) before the group stop reaches it. The parent sees WIFSTOPPED only after every thread has stopped, so the test saw a stopped `cctg run` with the terminal already raw again. The only other writers of the user's termios are `start` and this `raw()`, and fake claude's stdin is its own pty, so nothing else can explain a raw mode while stopped. macOS passed this step because XNU gives a self-sent signal to the calling thread.
   Fix: `libc::raise(libc::SIGSTOP)`. It is thread-directed (`pthread_kill(self)`), so this thread takes the stop before it returns. POSIX says a stop signal stops the whole process. Still SIGSTOP (the planner's reason for SIGSTOP over SIGTSTP stands). The comment says why `kill(getpid())` was wrong.

2. **macos, `run_pty_e2e.rs:248` `left: -1, right: 0`** (test harness, `crates/cctg/tests/run_pty_e2e.rs`).
   Cause: `tcgetattr` on the test's slave fd returned -1. macOS (like the BSDs) revokes the controlling terminal's vnode when the session leader exits. `term.rs` already says so for claude's own pty. Here the session leader is `cctg run` (`setsid` + `TIOCSCTTY` on the test's slave). After it exits, every fd to that slave is dead, including the test's own. Which of the four `raw()` calls failed is not in the log. The run took 13.5 s, while the Linux run reached line 489 in 5.7 s, and the first check (line 416) would have failed within about a second. So it was the call after the exit (the last check, "the terminal mode is restored"). Before that exit the slave is valid on macOS. Linux does not revoke, so it does not fail there.
   Fix: the harness reads the mode through the pty master. Linux `tty_mode_ioctl` reads the slave's termios for `TCGETS` on a master (`real_tty = tty->link`). On BSD/XNU both ends share one `struct tty`, and `ptcioctl` falls through to `ttioctl` for `TIOCGETA`. The master is not the revoked vnode, and closing or revoking the slave does not reset `t_termios`.

3. **Failure messages print state** (as the prompt asked). `Terminal::raw() -> bool` became `Terminal::expect_raw(raw, what)`. On failure it prints the step name and `tcgetattr: <last_os_error>` (errno captured right after the call), or `c_lflag {:#x}` with the `ICANON|ECHO` mask plus the terminal tail. `openpty`, `TIOCSWINSZ` and `waitpid` asserts now print `last_os_error()`. The four `assert!(…raw())` call sites use `expect_raw` with the same step names.

Windows is unchanged: the edit in `term.rs` is inside the `#[cfg(unix)] mod host`, and `run_pty_e2e` is Unix-only (on Windows it prints "Linux and macOS only").

Confidence: cause 1 has one mechanism and the code shows it. For cause 2, the revoke is certain, but which call failed is inferred from timing. If the next CI run fails again, the new messages name the step and errno.

### Skipped

- The prompt's candidate list for macOS (`tcsetattr`/`TIOCSCTTY`/`TIOCSWINSZ`/`openpty` zero winsize): not the failing call (see above). No change.

### Test results

Windows host, `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:

- `cargo fmt --all -- --check`: exit 0 (after `cargo fmt --all` rewrapped the new lines).
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: `Finished`, exit 0.
- `cargo test -j 1 --workspace --locked --no-fail-fast`: exit 0, 824 passed, 0 failed; `run_e2e: ok`, `run_pty_e2e: Linux and macOS only` (`scratch/fixer-test.txt`).
- Unix cross check `scratch/reviewer2/unixcheck/unixcheck.sh C:/Users/user/dev/cctg <scratchpad>/unixmini` (clippy `-D warnings`, `-Zbuild-std`, `--lib --test run_pty_e2e`): `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-apple-darwin` all `Finished` with no diagnostics (`scratch/fixer-unixcheck.txt`). I checked that the mini crate had the edited `term.rs` (`raise(libc::SIGSTOP)`) and test (`expect_raw` x5).
- Not run here: `run_pty_e2e` itself. It needs the ubuntu/macos CI runners (next push).

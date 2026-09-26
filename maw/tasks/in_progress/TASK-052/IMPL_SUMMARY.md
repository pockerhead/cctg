# TASK-052 implementer summary

Pre-flight: small-fix mode, no plan. Every entity the spec and orchestrator notes name exists with the assumed shape: `install.sh` (PATH note at the end of `main`, the macOS `CCTG_HOST` branch in `write_device_env`, `strip_device_env` and `uninstall_all`), `crates/cctg/tests/install_e2e.rs` (sh or Git Bash with a temp HOME), and in `hub/slots.rs` the hand-over in `flush`, the 👀/✍ receipts through `stream::receipt_parts` and `Step::Working` in `on_chunk`, `Activity` with `busy()` from the UserPromptSubmit/Stop/tool hooks, and the TASK-048 bursts. Code commit: `d5e38de` on `fix/install-path-channel-off`.

## 1. What was implemented

| File | Lines | What |
|---|---|---|
| `install.sh` | +138 / -12 | PATH line, container host name, `--host`, uninstall of the PATH line |
| `crates/cctg/src/hub/slots.rs` | +211 / -1 | channel-off detection and its notice, 2 unit tests |
| `crates/cctg/src/hub/mod.rs` | +1 | `channel_wait: slots::CHANNEL_WAIT` in the hub's `Options` |
| `crates/cctg/tests/install_e2e.rs` | +91 / -2 | PATH line in the device test, new container test, bad `--host` case |
| `README.md` | +1 / -1 | one sentence on the PATH line and `--host` |

### Installer (`install.sh`, POSIX sh, LF, mode 100755 kept)
- `PATH_LINE='export PATH="$HOME/.local/bin:$PATH" # cctg'`, built in double quotes with escapes so shellcheck raises no SC2016.
- `offer_path` replaces the old "not in PATH" note. It runs only when `~/.local/bin` is not in `PATH`. It picks the file by `$SHELL` in `rc_file`: `zsh` gives `~/.zshrc`, `bash` gives `~/.bashrc`, anything else gives `~/.profile`, and Windows (Git Bash) always gives `~/.bashrc`.
  - If the line is already in that file, it only prints a note.
  - With `--yes` it adds the line without asking. With a terminal it asks `[Y/n]`. Without a terminal and without `--yes` it prints the old note.
  - It only appends. If the last line has no newline, it adds one first.
- `strip_path_line` runs on `--uninstall`. It removes exactly `PATH_LINE` (`grep -v -x -F`) from `~/.zshrc`, `~/.bashrc` and `~/.profile` and writes each file in place (`cat >`), so a symlinked rc file stays a link. No other line changes.
- `--host NAME`, handled by `choose_host`, which runs at the end of `read_settings`, before any download or write:
  - `NAME` is checked against `[A-Za-z0-9._-]`.
  - It writes `CCTG_HOST=NAME` under a `(--host)` mark and replaces every `CCTG_HOST` line and mark already in device.env.
  - Running it again with the same name gives the same bytes.
- Container: `in_container` looks for `/.dockerenv`, `/run/.containerenv`, an env `container=` variable, or docker/containerd/kubepods/libpod/lxc in `/proc/1/cgroup`. When device.env has no `CCTG_HOST` line:
  - With a terminal it asks for a name. The suggestion is env `CCTG_HOST`, else the hostname, else `container` when the hostname looks like a hex container id.
  - With `--yes` or without a terminal it takes env `CCTG_HOST`, or prints a warning and writes nothing.
- The macOS branch stays as it was, with the same mark text, so an existing macOS device.env is not rewritten.
- All host marks now share the prefix `HOST_MARKED`. The `strip_device_env` awk matches that prefix, so uninstall also removes the container and `--host` lines.

### Hub (`hub/slots.rs`)
- New constants: `CHANNEL_WAIT` (20 s) and `CHANNEL_OFF_NOTICE`. The notice (in Russian) says the channel seems off because claude was started without `--dangerously-load-development-channels` (not through claude-cctg) or the dialog was not confirmed. It tells the user to run `/exit`, then `claude-cctg --continue` in the same folder, confirm the dialog, and send earlier messages again.
- New `Options::channel_wait`. The default is `ZERO` (off), so existing tests are unaffected. The hub sets `CHANNEL_WAIT`.
- In `flush`, after a text inbound is handed over (`mark_handed`), `expect_taken` starts a watch only when all of these hold:
  - `channel_wait` is not zero;
  - the session is not `busy` (no turn is running, so a message queued during a turn never counts);
  - the session was not told already;
  - the session has a stream target (an agent with `transcript_reads` bound to it).

  The watch is `unseen: session -> first hand-over time`.
- Signs that the message was taken clear the watch:
  - in `on_chunk`, any `StreamItem::Channel` record (this also ends the "told" state) or a `Step::NewTurn`;
  - in `on_hook`, `UserPromptSubmit`, `ToolStart` (PreToolUse) or `Stop`.
- `check_channel` runs from `on_chunk` when a read reached the end of the transcript (`!more && !stopped`, or a missing file). If `channel_wait` has passed since the hand-over, it sends one notice through `send_messages` (which respects the 256 cap) and marks the session as told. A told session is not watched again until one of its channel records shows up.
- Session end or reaping drops both states (the same `retain` as `activity`).
- There is no new task. The check runs on the stream's own read deadline (`next_read` in `next_deadline`, every 300 ms).

## 2. Deviations and choices (also in log.jsonl as `decision`)
- The check does not fire from a separate `on_tick` deadline. It needs a stream read that reached the end of the file after the wait. A stream that lags behind Telegram backpressure (reads paused, `more`/`stopped`) never produces a false notice. The orchestrator asked for "the actor's deadline mechanism"; the stream's read deadline is that mechanism here.
- Only streamed sessions are watched. Without a stream the hub cannot see channel records, and whether `UserPromptSubmit` fires for channel messages is unverified, so watching them would give false notices.
- Only text inbounds start a watch; file hand-overs do not. The next text message catches the same problem.
- The test fakes a container with env `container=docker`, which Podman and systemd-nspawn really set. There is no test-only override in the script.

## 3. Tests
- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace` (shared target, `CARGO_PROFILE_DEV_DEBUG=0`): exit 0, 41 test binaries, 841 passed, 0 failed, 3 ignored. Log: `scratch/workspace_test.log`.
- New or changed tests:
  - `hub::slots::tests::a_message_nobody_takes_tells_the_topic_once_until_one_is_taken`: a fake reader link, a message with no record gives one notice after the wait and not before. More lost messages give no second notice. A channel record resets the state, and the next lost message gives a second notice.
  - `hub::slots::tests::a_message_during_a_turn_or_one_taken_tells_nothing`: a message during a turn, a message whose record shows up, and a message followed by UserPromptSubmit give no notice.
  - Mutation check: with `busy()` ignored, the second test fails as it should (`scratch/mutation_busy_check.sh.txt`).
  - `install_e2e::install_update_and_uninstall_a_device`: the PATH line goes into `~/.zshrc` (Windows: `~/.bashrc`) once after the user's own lines, the rerun leaves the file's mtime unchanged, and uninstall restores the original bytes.
  - `install_e2e::a_container_gets_its_host_name_written`: covers the warning without a name, `CCTG_HOST` from the env, an existing line being kept, `--host` replacing both our line and the user's, an idempotent rerun, and uninstall removing device.env.
  - `install_e2e::missing_or_bad_settings_without_a_terminal_write_nothing`: new case `--host "my box"` fails before anything is written.
- shellcheck 0.11.0 (`pip install --target <session scratchpad>/sc shellcheck-py`; Docker was not running): `--shell=sh`, `dash` and `bash` report nothing on `install.sh`.

## 4. How to verify by hand
- Installer on Linux or macOS without `~/.local/bin` in PATH:
  1. Run `sh install.sh --yes ...`: `~/.zshrc` (or `~/.bashrc` / `~/.profile` by `$SHELL`) ends with `export PATH="$HOME/.local/bin:$PATH" # cctg`.
  2. Run it again: still one line.
  3. Run `sh install.sh --uninstall`: the line is gone and the rest of the file is unchanged.
- In `docker run -it ...`: the installer asks "This is a container ... Name for the topics". With `--yes` and `-e CCTG_HOST=box`, device.env gets `CCTG_HOST=box`. `--host NAME` replaces it.
- Hub: start claude without the channel flag (plain `claude` with the user-scope server, or decline the dialog), then send a message to its topic. About 20 s later the topic gets the notice, once. After `/exit` and `claude-cctg --continue` with the dialog confirmed, a message gets ✍. A message sent during a running turn never gives the notice.

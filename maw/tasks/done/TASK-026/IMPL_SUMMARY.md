# TASK-026 implementer summary

Mode: small-fix (no plan; implemented from the spec). Code commit: `26ebca4` on `feature/hub-supervisor`.

## 1. What was implemented

Design as built:

- **Graceful hub stop.** `cctg hub` now stops on Ctrl+C, and with `--stop-on-stdin` also when its stdin reaches EOF (a plain std thread reads stdin to the end). Polling stops only at the `getUpdates` await or a backoff sleep (`updates::poll_until`), never between saving an offset and handing its batch out. Then `Control::Stop` goes to the slot actor behind every message the poll already routed; the actor handles hook posts and agent frames still queued, pumps, drops its save channel and awaits the save task, so the last registry snapshot is on disk (`slot registry saved`). The hub waits at most 10 s for that.
- **`cctg supervise [--env-file P] [--trial-secs 10]`.** Paths are fixed at start from its own exe (`<bin>/cctg.exe`, `cctg.next.exe`, `cctg.next.exe.part`, `cctg.old.exe`, `cctg.bad.exe`, `cctg.deploy-result`). The hub runs as `cctg hub [--env-file P] --stop-on-stdin`, with stdin piped, in its own process group (Windows `CREATE_NEW_PROCESS_GROUP`, Unix `process_group(0)`), so console Ctrl+C reaches only the supervisor. A hub that exits is restarted after 1, 2, 4 ... 60 s; the backoff resets after 60 s of uptime or a successful deploy. Ctrl+C / Ctrl+Break (SIGTERM on Unix) closes the hub's stdin, waits up to 30 s (then kills), and exits 0. The supervisor never reads the env file; logs carry file names, pids, exit codes and the version line only.
- **Candidate handling** (checked every second while a hub runs, and before every start, so a crash-looping hub can still get its fix): `cctg.next.exe --version` must exit 0 within 10 s and print `cctg ...`; bytes equal to the running binary give `unchanged`. Otherwise: stop the hub gracefully, sweep old set-aside files, set `cctg.old.exe` aside (delete, or rename to `cctg.old.exe.<nanos>` when a running process holds it), rename `cctg.exe` -> `cctg.old.exe`, `cctg.next.exe` -> `cctg.exe`, start the hub, and wait the trial period. A hub that exits within it is rolled back: `cctg.exe` -> `cctg.bad.exe`, `cctg.old.exe` -> `cctg.exe`, restart. Renames retry 3 times (100/300/1000 ms). The result is written as `cctg.deploy-result` (temp + rename).
- **`cctg deploy <exe> [--bin-dir D] [--timeout-secs 120]`.** Refuses when `cctg.next.exe` already waits; removes an old result; copies to `.part`, renames to `cctg.next.exe`; polls for the result. Prints `deployed: cctg <ver>` / `unchanged: ...` (exit 0) or `rejected: ...` / `rolled back: ...` / `failed: ...` (exit 1). On timeout it withdraws a candidate nobody took.
- **`CCTG_BOT_API_URL`** (optional, must start with `http://`/`https://`, default Telegram): lets the real-process test run hubs against a fake Bot API. `BotApi::with_api_url` already existed.

Files (diff vs `main`, +1618 / -18):

| file | change |
|---|---|
| `crates/cctg/src/supervise.rs` | new, 694 lines (module + 5 unit tests) |
| `crates/cctg/tests/supervise_e2e.rs` | new, 621 lines (real-process scenario, `harness = false`) |
| `crates/cctg/src/main.rs` | +96: `supervise`, `deploy`, `hub --stop-on-stdin`, CLI parse tests |
| `crates/cctg/src/hub/mod.rs` | +54: `stop_requested`, `poll_until`, `Control::Stop`, bounded wait |
| `crates/cctg/src/hub/updates.rs` | +39: `poll_until` (`poll` delegates with a pending stop) |
| `crates/cctg/src/hub/slots.rs` | +37: `Control::Stop`, drain + final save in `run` |
| `crates/cctg/src/hub/config.rs` | +30: `CCTG_BOT_API_URL` + test |
| `crates/cctg/Cargo.toml` | tokio features `process`, `signal`; Windows dev-dep feature `Win32_System_Console` for the test; `[[test]] supervise_e2e` |
| `Cargo.lock` | +21: `signal-hook-registry`, `errno` (Unix-only, pulled by tokio `signal`) |
| `crates/cctg/src/lib.rs` | +1 |
| `docs/poc.md` | +30: section "Через supervisor" |

## 2. Deviations and limits

- Stop mechanism is the stdin pipe, not a file or a TCP/HTTP request with a secret (log entry). Only the parent can trigger it, and a supervisor that dies closes the pipe as well.
- "Version/hash differ" is a byte comparison: every build reports `cctg 0.1.0`, and no hash crate is in the agreed set.
- No new crates. tokio features `process` and `signal` were added; `signal` pulls `signal-hook-registry` + `errno` on Unix only.
- Ctrl+C during a trial run is honoured after the trial (at most `--trial-secs`), not at once.
- Between the two renames `cctg.exe` is missing for microseconds; a hook started exactly then fails once (non-blocking in Claude Code). During the hub downtime (about 1-3 s) hooks spool `SessionStart`/`SessionEnd`; a `Stop` answer that falls into it is lost (existing TASK-018 limit).
- The supervisor's own code changes only on its restart; a deploy replaces what the hub runs.
- A hub that fails its trial for a reason not in the binary (Telegram down at that moment) is rolled back too.
- (fixer, I3) Telegram jobs already handed to the dispatch task when the hub stops are cut: a subagent block's first send becomes its TASK-015 tombstone, a `createForumTopic` in flight can leave an orphan topic (TASK-011 limit), stream lines are resent. No worse than the old hard Ctrl+C; documented in docs/poc.md.
- (fixer, I6) A half-failed rollback (no `cctg.exe`) is now repaired by the next deploy: a missing `current` counts as "different", and `swap_in` leaves `cctg.old.exe` alone then.
- (fixer) A keyboard Ctrl+Break during a deploy trial may reach the hub (same console); the hub now stops gracefully on it, but the supervisor reads that as a failed trial and rolls back. Documented.
- Not tested: stopping a hidden supervisor with `taskkill /F` (by design its hub sees stdin EOF and stops gracefully). Unix-only code (`process_group`, SIGTERM, `kill -TERM` in the test) sits behind `cfg(unix)` and was neither compiled nor run on this Windows host.

## 3. Test results

With `CARGO_TARGET_DIR=%TEMP%/cctg-t026-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: all green. cctg lib 414 passed / 1 ignored, main 1, all integration binaries ok, `supervise_e2e: ok`, transcript suites ok; soak skipped as before. About 80 s total.
- `supervise_e2e` (real processes, fake Bot API, temp home/state, no window) checks in one run: two failing starts restarted after >=1 s and >=2 s; SessionStart -> one topic, a real `cctg agent` receives a topic message; `deploy` of a different build -> `deployed`, binary swapped, `cctg.old.exe` = original, no second `createForumTopic`, registry still has the session, the agent reconnects and receives the next message; a candidate whose hub exits 3 -> `rolled back`, exit 1, previous binary back, `cctg.bad.exe` kept, the agent gets a message again; a non-program -> `rejected`, nothing changed; the same bytes -> `unchanged`; Ctrl+Break -> supervisor exits 0, the hub logged `slot registry saved` again, the hook port is closed; no secret or token in the logs.

## 4. Manual verification

Switch the live hub (orchestrator; after merge, from the main tree):

```
cargo build --release -p cctg
# stop the running hub (Ctrl+C in its window); the old build has no graceful stop,
# registry writes are atomic, at most its very last change is lost
mv ~/.cctg/bin/cctg.exe ~/.cctg/bin/cctg.old.exe      # running agents hold it: rename, never overwrite
cp target/release/cctg.exe ~/.cctg/bin/cctg.exe
cd <the folder the hub ran from: .env and .cctg/ are there>
~/.cctg/bin/cctg.exe supervise --env-file <that folder>/.env
```

Deploy afterwards:

```
cargo build --release -p cctg
~/.cctg/bin/cctg.exe deploy target/release/cctg.exe
```

Expect `deployed: cctg 0.1.0` and exit 0; the hub log shows `installing a new hub binary`, `hub stopped`, `hub started, polling`; topics and routing continue.

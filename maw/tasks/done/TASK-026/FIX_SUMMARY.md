# TASK-026 fix summary

Code commit: `f1fb788` on `feature/hub-supervisor` (on top of `ac74ddd`).

Preflight: `scratch/` held only `.gitignore`. The claim I checked first, because it would break correct code if it were wrong, was I2: "close the receivers and ingress answers 503". I verified it in the code. `handle_hook` (ingress.rs:460-467) maps every `try_send` error to `Status::Unavailable` (503) and records dedup only after `Ok`. The agent reader ends its connection when `events.send` fails (ingress.rs:77/99), and tokio `Receiver::close` still delivers messages that are already buffered. So the fix is safe, and the bug it fixes is real.

## 1. Fixed

- **I1 (major), failed swap keeps the candidate.** Confirmed at supervise.rs `install`: the `Failed` path left `cctg.next.exe`, and the loop picked it up again on every start and tick. Fix: new `withdraw()` runs after unchanged, rejected and failed swap. It deletes `next`, or renames it to `cctg.bad.exe.<nanos>`, which the sweep deletes later. A `next` that still cannot be removed is stamped by size and mtime (`stuck`). `waiting()` does not take it again until the file changes, and the supervisor logs one warning to delete it by hand. Tests: unit `a_refused_candidate_leaves_the_next_name` and `a_failed_swap_keeps_the_running_binary` (Windows; `cctg.old.exe` held with `share_mode(0)`). e2e step 6b (Windows) holds `cctg.old.exe` open, deploys another build and checks: `failed: cannot delete or rename the previous cctg.old.exe ... (os error 32)`, exit 1, `next` withdrawn, `cctg.exe` unchanged, exactly one hub restart (`getMe` count stable for 4 s).
- **I2, hook lost with 204 during the final save.** Confirmed. `Slots::run` now calls `hooks.close()` and `agents.close()` before the `try_recv` drain. A hook in the stop window gets 503 and spools. There is no dedicated test: I could not find a cheap way to hold the save open deterministically. The behaviour relies on the documented tokio `close` semantics plus the ingress mapping above.
- **I4, Ctrl+Break / SIGTERM to the hub.** `supervise::stop_signal` is now `pub(crate)` and returns the signal's name. `hub::stop_requested` uses it, so the hub stops gracefully on Ctrl+C, Ctrl+Break (Windows) and SIGTERM (Unix). The supervisor logs `stopping signal=...`. I did not verify with a live console that a keyboard Ctrl+Break reaches the hub's group. The change is correct either way.
- **I5, deploy result bound to its deploy, plus a lock.** `deploy` takes an OS lock with `File::try_lock` on `cctg.deploy-lock`, which is released however the process ends. A second deploy gets "another cctg deploy is running". It writes an id (`<pid>-<nanos>`) to `cctg.deploy-id` before the candidate appears. `take_candidate` reads the id and removes the file, and `report` echoes the id. The result format is now `tag\nid\ndetail\n`. `deploy` accepts only a result carrying its own id. On timeout it withdraws both `next` and the id. Test: `deploy_takes_only_its_own_result_and_runs_alone` checks that a foreign result is ignored, its own result is taken, and a held lock refuses a deploy.
- **I7, CCTG_BOT_API_URL.** It now accepts `https://` anywhere, but `http://` only to `localhost`, `127.x` or `[::1]`. It rejects spoofs such as `127.0.0.1.example.org` and `user@host`. The hub logs one warning when the URL is not the Telegram default, without the URL. The doc comment and docs/poc.md say it is test-only. The config test was extended.
- **I6 (a few lines, so fixed rather than only documented).** `check_candidate` treats a missing `cctg.exe` as "different". `swap_in` without a `current` leaves `cctg.old.exe` alone and just moves the candidate in. Test: `without_a_current_binary_the_old_one_is_kept`.
- **Nit, Ctrl+C during the trial.** docs/poc.md now says it is honoured after the trial. It also says that a keyboard Ctrl+Break in the trial may reach the hub and cause a rollback.
- **Nit, `\r` in the detail.** `encode` now flattens `\r` too.

## 2. Skipped / documented only

- **I3, in-flight Telegram jobs cut at exit.** Documented in the IMPL_SUMMARY limits and in docs/poc.md, as the task scope asked. A bounded wait for `done` results would touch the dispatch/actor lifetime, which is beyond a few lines. It is no worse than the old hard stop.
- **Nit, lazy stdin reader thread.** Harmless; the supervisor's 30 s kill bounds it.
- **Nit, `Unchanged` resets the backoff.** Harmless, not changed.
- **Missing coverage, `--version` hang or wrong output, a deploy during a crash loop, Unix compilation.** Not added. There is still no Linux target on this host, so the `cfg(unix)` paths (SIGTERM in `stop_signal`, now also used by the hub) are still neither compiled nor run. That remains a check for CI or a Linux box before step 5.
- **The e2e needs a console for `GenerateConsoleCtrlEvent`.** Unchanged.

## 3. Test results

`CARGO_TARGET_DIR=%TEMP%/cctg-t026-fixer-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`. The target dir was deleted afterwards.

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: all green. The cctg lib has 418 passed and 1 ignored (was 414; 4 new supervise tests). main has 1, and every integration binary is ok. `supervise_e2e: ok`, including the new step 6b (`deploy: failed: cannot delete or rename the previous cctg.old.exe: ... (os error 32); the old binary runs`). Transcript suites are ok, and soak was skipped as before.

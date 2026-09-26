# TASK-056 IMPL_REVIEW (code-reviewer, claude opus, medium)

Reviewed: `git diff 7748a43 9b7b101` (merge base with main to the branch commit). Main has moved since (TASK-053, `4c642fb`), so a plain `git diff main 9b7b101` also shows TASK-053 reversed; the branch touches only `statusline.rs`, `statusline_cli.rs`, `install_e2e.rs`, `docs/poc.md`. `git merge-tree --write-tree main 9b7b101` is clean (rc=0).

## 1. Verdict

**SHIP.** The two lines match `statusline.py` for real inputs. The email stays in-process and goes only to stdout. git runs with a timeout and the POST and chaining paths are unchanged. The remaining differences are minor and listed below.

## Disconfirmation

Counter-example tested first: the `git` child is spawned without `CREATE_NO_WINDOW` (`statusline.rs` `git()`: no `creation_flags`, unlike `shim.rs:197`), so each status line refresh could flash a console window on Windows.
Result: **did not hold as a defect.** `cctg statusline` is a console child of the claude terminal (through Git Bash), so `git.exe` inherits that console. The user's own `statusline.py` spawns git the same way (no flags) and shows no flash. The pre-existing `run_chained` shell spawn has no flags either. This is hardening only (finding 5), not a bug.

## 2. Confirmed correct (with proof)

- Format and ANSI match the reference. Repro in %TEMP% with a freshly built binary, same input through both:
  - cctg: `^[[1mOpus^[[0m [high] dir:plain ctx:95%` / `acc:x@y.test ^[[31m5h:95%^[[0m ^[[33m7d:81%^[[0m`
  - py: `^[[1mOpus^[[0m [high] dir:plain ctx:94%` / `acc:x@y.test ^[[33m5h:94%^[[0m ^[[33m7d:80%^[[0m`
  Bold, cyan, yellow and red codes, the spacing, `[effort]`, `dir:`, the order and the second line appearing only when non-empty are all the same. The difference is rounding at .5 (finding 1).
- `sh:` and the `statusline_last_input.json` dump were left out, as the spec says (`own_line`, `statusline.rs`).
- Email isolation. `account_email` is called only in `own_output`, which runs only when there is no chained command (`run()`: `match chained { Some(..) => .., None => own_output(..) }`). It never reaches the env of the user's command or the `HookPost` (built earlier from `event()`, which is unchanged). There is no `tracing` call on it. `email_of` rejects control characters (ESC injection test) and lengths over 254. The CLI test checks the serialized POST body and `Debug` of the received `HookPost`, plus stderr.
- `CLAUDE_CONFIG_DIR` handling in `claude_json_path` mirrors `settings_path` exactly (the same trim and empty filter, and the same `device::home_dir` fallback).
- git edge cases (repro, fresh binary, temp HOME):
  - missing cwd `C:/definitely/not/here`: `dir:here`, no branch, rc 0.
  - `PATH=/nonexistent` inside a repo: no branch, rc 0.
  - not a repo (with `GIT_CEILING_DIRECTORIES`): no branch.
  - in the worktree: `br:feature/builtin-statusline*`, wall time 0.204 s for the whole debug run.
- `--no-optional-locks` is kept, stdin and stderr go to null, the child uses `kill_on_drop`, and `tokio::time::timeout(GIT_TIMEOUT)` wraps each call. Both calls run under `tokio::join!`, so the worst case is about 400 ms.
- There are no new crates (`serde::Deserialize` was already a dependency). `own_line` is pure and all IO is in `own_output`, which is testable.
- `cargo fmt --all --check` ok. `cargo clippy -j 1 --workspace --all-targets -- -D warnings` ok. `statusline_cli` passed 4 of 4 runs (6 tests each).
- install investigation: I accept the no-hole conclusion. `install_e2e` already asserted the `"<exe>" statusline` command, and the new `type == "command"` assertion is cheap and correct.

## 3. Issues

1. **minor**, `crates/cctg/src/statusline.rs` `percent()`, with `own_line` limit colouring. Values at .5 round half away from zero, and Python rounds half to even. This changes the colour at the thresholds. Verified with input 94.5: cctg `\x1b[31m5h:95%` (red) against py `\x1b[33m5h:94%` (yellow). 80.5 gives `7d:81%` against `7d:80%`. It is deliberate and documented in IMPL_SUMMARY (consistent with the hub numbers), but it is not "identical to statusline.py". Suggested fix: none required. If exact parity is wanted, use `round_ties_even()` (stable since Rust 1.77) for the terminal line only.
2. **minor**, `percent()`. Values outside 0..=100 are dropped (for example `ctx 100.6`), and py prints `101%`. This is old behaviour, documented, and the field is documented as 0..100. Leave it.
3. **minor**, `git_branch` with `GIT_TIMEOUT`. In a large repo where `git status --porcelain` takes over 400 ms, the line shows the branch **without** `*`, which reads as a clean tree while it is dirty. Every refresh also starts a full status scan that is then killed. py has the same semantics with 3 s, so a false "clean" is rarer there. Suggested fix, optional: a short follow-up, for example `-uno` or a distinct marker on timeout. Not blocking, since the spec asks for a short timeout.
4. **minor**, `git()`. `GIT_*` env from the caller leaks in. Repro: `GIT_DIR=<cctg-056>/.git` with `current_dir` in a plain temp folder prints `br:feature/builtin-statusline*` for a folder that is not a repo. py does the same, and Claude Code is not known to set `GIT_DIR`. Suggested fix, optional: `.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE")` on the git command. The same leak would break the new CLI test when `cargo test` runs from inside a git hook.
5. **minor (hardening)**, `git()`. There is no `creation_flags(CREATE_NO_WINDOW)` under `#[cfg(windows)]`, unlike `shim.rs:197`. It is harmless today (see Disconfirmation). It becomes a flash if Claude Code ever spawns the status line without a console. The fix is three lines and matches `shim.rs`.
6. **nit**, `folder_name("C:\\")` returns `C:` and py shows `C:\` (ntpath `basename("C:")` is empty, so py falls back to the full cwd). The model name is trimmed and cut to 64 characters (`text()`); py prints it raw. A non-string `effort.level` is dropped; py prints it. These are cosmetic.

Not verified: on Windows with PATH resolving to `Git\cmd\git.exe` (the wrapper), killing it on timeout may leave the real `mingw64\bin\git.exe` running. Under Git Bash, `/mingw64/bin/git` is first on PATH (checked with `which -a git`), so the normal path has no wrapper. My probe of the wrapper did not start a child, so this stays unconfirmed.

## Flaky failures (are they new?)

In three workspace runs of mine, none of the failures points at this change:
- `transcript --test purity::every_source_file_is_scanned` failed twice with `read_dir` NotFound. The test binary in the shared `C:/Users/user/dev/cctg/target` had been rebuilt from **another worktree** whose `CARGO_MANIFEST_DIR` no longer exists. Running it alone recompiled `transcript` and it passed 3/3. `crates/transcript` is untouched by this branch.
- `hook::tests::a_failed_kept_event_stops_the_hook_within_its_budget` failed once at 1.116 s against a 1 s bound, while other agents were building in parallel. `hook.rs` is untouched.
- One suite of 6 tests failed once after 60.07 s in the first run, and that output was not kept, so I don't know which suite it was. The candidates are `question_hook_e2e`, `reads_e2e` and `statusline_cli`. `statusline_cli` then passed 4 of 4 runs.
- The implementer's cold-run `statusline_cli` 80 ms POST miss and `hook_cli` 500 ms miss are timing bounds under load, and the `cat` case does not run git. The shared target also means `CARGO_BIN_EXE_cctg` can be overwritten by a sibling worktree's build between build and test. These are not new with this change.

## 4. Missing coverage

- No test for the git timeout path. A fake slow `git` (a test helper binary first on PATH) would prove the line comes back in about 400 ms without the branch. The `took < 3 s` bound in the CLI test does not exercise it.
- No `.5` rounding test at a colour threshold. It would pin the chosen rounding (finding 1) so it does not change silently.
- No test that `CLAUDE_CONFIG_DIR` is honoured end to end in the CLI (only the unit test of `claude_json_path`).
- No test with `GIT_DIR` set in the environment (finding 4), if the fix is taken.

## 5. Nits

- The module doc line `//! its own ([`own_line`], TASK-056). It exits ...` is now longer than its neighbours; this is fmt-clean but reads oddly.
- `git_branch` and `claude_json_path` are `pub` only for tests in the same module. `pub(crate)` or private would do.

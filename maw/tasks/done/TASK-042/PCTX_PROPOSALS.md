# PCTX proposals (TASK-042)

## 2026-09-24 implementer: test processes of `cctg` go through `tests/common`

Proposed risk lesson (hub or channel domain):
- 2026-09-24 (TASK-042): cargo tests run inside a live Claude Code session on a machine with a live hub. A `cctg agent` started by a test that inherits `CLAUDE_CODE_SESSION_ID` and reads the real `~/.cctg/device.env` registers with the live hub as the developer's session (the `stdout.rs` test did this; `.output()` gives it a null stdin, so it lived ~16 ms and unbound the real agent). Every test that starts `cctg` uses `tests/common::{cctg, isolate}` (drops all `CCTG_*` and `CLAUDE*` vars, own `HOME`/`USERPROFILE`); `tests/isolation.rs` enforces it.

Why: without the rule a new e2e test can repeat the leak; the guard catches files that start `cctg` without the helper.

## 2026-09-24 implementer: shared CARGO_TARGET_DIR and `CARGO_BIN_EXE_cctg`

Proposed addition to the build rule: two worktrees that share one `CARGO_TARGET_DIR` overwrite each other's `target/debug/cctg.exe` (the uplifted binary that `CARGO_BIN_EXE_cctg` names). e2e tests then run the other branch's binary (seen: `client_version` in spool records, `--trial-secs` unknown). Touch a source file of the crate and rerun before trusting an e2e failure, or give each worktree its own target.

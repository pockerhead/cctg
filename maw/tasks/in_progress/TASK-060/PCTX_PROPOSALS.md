# PCTX proposals (TASK-060)

## 2026-09-26, implementer: universal risk lesson
The shared target dir (`CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target` for every worktree) makes `env!("CARGO_TARGET_TMPDIR")` shared too. An integration test that keeps a home (`device.env` with its own hub port, a spool) at a fixed name there collides with a concurrent run of the same test binary from another worktree, and a spool left by a failed run is replayed into the next run. Reproduced: two concurrent `hook_cli` / `statusline_cli` runs fail 2 of 10 and 4 of 10. Rule: per-process homes go under `common::own_tmp()` (`<CARGO_TARGET_TMPDIR>/<pid>`) or `temp_dir()` + pid, never under a bare test name.

## 2026-09-26, implementer: hub risk lesson
A test that presses a button as soon as the fake Telegram records the send races the hub: the send's `Delivery` (message id) reaches the slots actor on a different channel than `Control::Callback`, and `select!` picks either one. Question buttons now also match the one in-flight ask with that id (`Asks::in_flight`). Permission prompts (`prompts.by_message`) still have the same window. `permission_hook_e2e` presses right after the send is recorded, so it is exposed to the same flake.

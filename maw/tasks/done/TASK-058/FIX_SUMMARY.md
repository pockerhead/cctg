# TASK-058 fix summary

Worktree `C:/Users/user/dev/cctg-058`, branch `fix/statusline-via-agent`, on top of `be4a18e`.

Preflight check (the review claim most likely to break things if applied verbatim): finding 4's fix "call `tell_bound(conn)` again when a `StatusLine` for another session arrives". I checked it in `agent.rs` `StatusState::due`: after `/clear` the agent still watches the OLD session's file, and `cctg statusline` now writes only the NEW session's file. The old file does not change, so the agent sends no mismatched frame and that trigger never fires. Applied verbatim, it would leave the bug in place in exactly the case it targets. I used a tick retry instead (see 4).

## Fixed

1. **Change detection by mtime only (agent.rs `StatusState::due`, statusfile.rs).** The agent now reads the file on each 1 s tick (one file under 4 KiB) and compares the numbers with the last `HookEvent` sent on this connection (`sent: Option<HookEvent>`). mtime is no longer used. `statusfile::read` returns only the numbers, and `statusfile::changed` is gone because nothing used it any more (it was added by this task). New test `agent::tests::numbers_written_within_one_mtime_are_sent_too`: two writes forced to the same mtime (`set_modified`). The second numbers are sent, and the same numbers are never sent twice. With the old `self.sent == Some(mtime)` check, the second `due()` would return `None` and the test would panic.
2. **status_e2e did not prove the guard (tests/status_e2e.rs).** A second live top-level session (`...0293`, pid 11) is now started first, and its status message in topic 101 is awaited. Then A's link sends `numbers(other, 77)`. The test waits 500 ms after A's numbers are shown and asserts that `ctx 77%` never appears. The end-of-test "old agent" step reuses that session. Mutation check: I removed the `if session_id == session` guard in slots.rs and the test FAILED at status_e2e.rs:863 ("numbers of another session shown"). Then I restored the guard.
3. **Rollout order not documented.** `docs/remote-hub.md` got a sentence in the TLS status-line bullet and a bullet "Порядок обновления: сначала hub, потом устройства" under "Версии клиента и hub". `docs/poc.md` got one sentence after the statusline paragraph: update the hub before the clients, because a new agent with an old remote hub shows no numbers.
4. **`bound` lost on a full agent queue (hub/slots.rs `tell_bound`).** `Conn.untold` is set when `try_send` returns `Full` and cleared on success or `Closed`. `on_tick` re-tells every untold conn with its CURRENT session. `next_deadline` wakes the actor within `BOUND_RETRY` (1 s) while any conn is untold. The queue is FIFO and the retry always carries the current binding, so a stale `bound` can never overtake a newer one. New test `hub::slots::tests::a_bound_that_finds_the_queue_full_is_told_again`: capacity-1 queue, `/clear` rebind while the queue is full, `untold` set, deadline at most 1 s away, tick delivers `Bound(B)` exactly once.

## Skipped

- **Finding 4's specific trigger (re-tell on a mismatched `StatusLine`):** not used, because it is ineffective (see Preflight). The tick retry covers that case and more.
- **Test for keeping the files over a worker handover:** not added. The only branch is `!matches!(ended, Ok(Ended::Handover))` in `run_stdio`. A meaningful test needs a real worker handover (a new exe, the shim, an update round), which is not cheap. A unit test of the `matches!` would only restate the code.
- **Review nits** (tell_bound for pending/nested sessions, a crashed agent's mark suppressing the fallback for up to 120 s, serial file I/O in `hand_over`): the review marks them harmless or accepted, and I agree. No change.

## Test results

Shared target `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, with `lib.rs`, `main.rs` and `tests/status_e2e.rs` touched first.

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace --no-fail-fast`: exit 0, all 45 test binaries ok (lib: 699 passed, 1 ignored), no flakes this run. Full output is in `scratch/fixer_test_workspace.txt`.
- Guard mutation run: `cargo test -p cctg --test status_e2e numbers_over` with the guard removed gave `FAILED` (panicked at status_e2e.rs:863). The guard was restored before the full run.

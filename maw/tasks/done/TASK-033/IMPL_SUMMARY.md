# TASK-033 implementer summary

Mode: small-fix. Pre-flight: pointers checked (Block, block_work, block_done in `hub/registry.rs`; on_block_done/pump in `hub/slots.rs`; Op::Send in `hub/scheduler.rs`; `send_message` in `hub/api.rs`). Only gap: Op::Send and `send_message` had no reply parameter, added.

## 1. What was implemented

- `crates/cctg/src/hub/api.rs` (+6): `send_message(.., reply_to: Option<i64>)` sets `reply_parameters: {message_id}` (no `allow_sending_without_reply`: a reply to a deleted block is refused, not sent loose).
- `crates/cctg/src/hub/scheduler.rs` (+17/-4): `Op::Send` gets `reply_to: Option<i64>`, passed through by the `BotApi` transport; stream sends pass `None`.
- `crates/cctg/src/hub/registry.rs` (+177/-14):
  - `Block.notified: bool` (persisted, `serde(default)`, skipped when false): the finished reply was handed out; never again.
  - `BlockNotice { thread_id, reply_to, text }`.
  - `Registry::block_done` now returns `Option<BlockNotice>`: the first time the block shows its final text (not running, nothing pending) and has both `thread_id` and `message_id`. Text `✓ <type> <short id> закончил`, or `✗ <type> <short id> итог не получен` when the applied text is exactly `<header>\nитог не получен`. Type comes from the `↳ <type> <id>` header (fallback `agent`), `nested` for nested runs; short id = 8 chars.
  - Test `a_finished_block_is_announced_once_by_a_reply`: subagent finish, later edit (no second), nested run incl. re-run and re-end (once), finished-before-first-send, tombstone (no message_id, no reply), restart (notified persisted, not re-announced; a running block lost after restart is announced ✗).
- `crates/cctg/src/hub/slots.rs` (+57/-21): `on_block_done` sends the notice via `send_messages` (metered `Op::Send`, counts toward the 256 pending cap, `Work::Message`) with `reply_to`. Marked notified at hand-out: a failed/dropped send is not retried (at most once). Tests updated: `after_a_restart_blocks_are_edited_never_sent_again` (only replies, one per block, ✗ for the lost one, ✓ for the late result) and `a_nested_run_shows_one_block_and_its_answer_only_there` (exactly one reply to the nested block).
- `commands.rs`, `tests/formatting_e2e.rs` (+1 each): `reply_to: None` in Op::Send literals.

## 2. Deviations / not implemented

- Nothing skipped. Not re-sent on failure by design (once-only); the notice is dropped if the 256-message cap is full.
- Blocks finished before this upgrade have `notified=false` but no pending work, so a restart does not announce them; a later new finish of such a block (e.g. nested re-run) would announce once.

## 3. Tests

`CARGO_TARGET_DIR=%TEMP%/cctg-task033-target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: all pass (cctg lib 441 passed, 1 ignored; all integration binaries ok; soak skipped as ignored by design; supervise_e2e ok; transcript suites ok).

## 4. Manual verification

With a live hub: run a subagent in a session; when its block is edited to the result, a separate message `✓ <type> <id8> закончил` appears as a reply to the block (tap shows the block). End the session while a subagent runs: `✗ … итог не получен`. Restart the hub: no replies for already finished blocks. A nested `claude -p` gives `✓ nested <id8> закончил` once.

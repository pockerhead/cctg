# FIX SUMMARY — TASK-021

## Fixed

- **Major 1 — late `Reply` could cross into a reused topic.** Added `Slots::live_reply_slot`, symmetric to `live_agent`. A reply is accepted only when the connection exists, its session is a live top-level session, `SessionEntry.agent == Some(conn)`, and that session is the slot's `current_session`. Rejected replies produce a fixed debug message without reply text. Verified that the existing pid-based `/clear` rebinding updates both the connection and registry before this check.
- **Major 2 — unknown slash-command value leaked into logs.** Removed the structured `command` field; unknown slash commands now emit only the fixed message `unknown slash command`. Updated the isolated `command_logs` integration binary (`.without_time()`) so its unique marker is inside the first slash token and is asserted absent from captured logs.
- **Minor — stale `--settings` documentation claim.** Removed the claim that the CLI reference says hooks are excluded from `--settings`. Kept `<CLAUDE_CONFIG_DIR>/settings.json` as a troubleshooting fallback.
- **Missing reply-routing coverage.** Added regressions proving that: an ended session's late reply cannot enter a slot reused by another session while the new session can reply; a stale duplicate connection cannot reply; an old connection without pid rebinding cannot reply after `/clear`; and a multi-chunk reply at backlog 255 is rejected atomically rather than partially queued.

## Skipped

- No actionable issue or requested missing-coverage item was skipped.
- The review's line-count nit about the historical `IMPL_SUMMARY.md` was not changed: it does not affect runtime behavior, and the required fixer artifact is this report rather than a rewrite of the prior stage's report.
- The review's already-confirmed-correct areas were left unchanged after verification.

## Test results

- `cargo fmt --all -- --check` — exit 0, no output.
- `CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --offline -j 1 --workspace --all-targets -- -D warnings` — exit 0; `Finished dev profile` with no warnings.
- `CARGO_PROFILE_DEV_DEBUG=0 cargo test --offline -j 1 -p cctg --lib reply` — exit 0; 12 passed, 0 failed.
- `CARGO_PROFILE_DEV_DEBUG=0 cargo test --offline -j 1 -p cctg --test command_logs` — exit 0; 1 passed, 0 failed.
- `CARGO_PROFILE_DEV_DEBUG=0 cargo test --offline -j 1 --workspace` — exit 0. The `cctg` library suite reported 237 passed, 0 failed, 1 ignored; every binary, integration test, transcript test, and doc-test also passed.
- `CARGO_PROFILE_DEV_DEBUG=0 cargo build --offline -j 1 --workspace` — exit 0; workspace build completed successfully.

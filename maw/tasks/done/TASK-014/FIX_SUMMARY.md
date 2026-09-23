# FIX SUMMARY — TASK-014

The codex fixer implemented the review items but could not finish its own final checks or write this file: after its builds Windows returned STATUS_DLL_INIT_FAILED for every new process (18 failed spawns in its event log). Its last message reports the new actor tests passing (56 passed). The orchestrator completed the stage.

## Fixed (by the codex fixer, verified by the orchestrator)

- **Major — a queued permission request could resurrect an ended session's prompt.** Permission requests for a known ended session are rejected before insertion, and prompt sending is conditional on the session still being live; the agent-before-SessionStart path for unknown sessions is kept. Agent frames are attributed to the session the connection was bound to when the frame was read, so a pre-`/clear` frame consumed after the rebind is not attributed to the new session. Regression tests added in `hub/slots.rs` (SessionEnd first, then the queued request: no permission send, no verdict; pre-clear frame belongs to A).
- **Minor — pruning could erase the evidence needed to close prompts.** Sessions ended or pruned inside the hook transition are closed before their registry entries disappear (test with `Registry::MAX_SESSIONS` reached: the old prompt ends `Closed` and its message is edited).
- Log tests updated for the new attribution (`tests/message_logs.rs`, `tests/permission_logs.rs`).

## Orchestrator completion

- `cargo fmt --all` applied (the fixer could not run it), then `cargo fmt --all --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace`: 371 passed, 0 failed.
- Stale `%TEMP%\cctg-*` build dirs and `target/debug/incremental` removed.

## Skipped

- Nothing from the review is known to be skipped; QA verifies each item independently.

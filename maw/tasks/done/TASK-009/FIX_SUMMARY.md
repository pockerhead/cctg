# FIX_SUMMARY — TASK-009

## Fixed

1. **Review #1 — silent delivery failures.** For every transcript-command delivery failure other than Telegram's `too long`, the worker now makes one best-effort attempt to send `Не удалось отправить транскрипт.` to the same thread. Failure of that notice is only logged and is not retried. Tests cover a non-size API error, continued handling of the next command, and a rejected fallback document without another fallback cycle.
2. **Review #2 — digits-only session prefixes were unexplained.** `USAGE` now states that a digits-only prefix needs an explicit prompt count and gives `/brief 3 2026` as the example. The parser behaviour is unchanged and the usage text is pinned by a test.
3. **Review #3 — synchronous offset persistence blocked the async worker.** Every `OffsetStore::save` attempt now runs through `tokio::task::spawn_blocking`. Save-before-dispatch ordering and the existing 100 ms / 500 ms retry policy are unchanged. `OffsetStore` is cheaply cloneable for the blocking task.
4. **Review #4 — unknown slash commands disappeared silently.** `Parsed::NotOurs` slash commands now produce a debug event containing only the first command word (for example `/sessions`). The remainder of the message, project paths, and user ids are not logged. The existing dedicated tracing integration-test binary still uses `.without_time()` and now verifies that the command word is present while a marker in the remaining text is absent.
5. **Review #5 — ambiguous-prefix replies leaked encoded project directories.** Candidate lines no longer include `Located.project`. Each line contains an eight-character session id, relative file age, and an optional `ai-title` found by reading at most the first 64 KiB of that transcript. The test uses a synthetic encoded private directory name and verifies that it is absent from every reply, while short ids, ages, and titles remain visible.
6. **Missing coverage requested by the review.** Added tests for `LocateError::RootUnreadable`, worker continuation after a failed delivery, and rejection of the document used by the one-time too-long fallback.

## Skipped

1. **Review #6 — directory fsync after rename.** Skipped per orchestrator decision because this task targets the Windows host. The existing temp-file `sync_all` plus rename behaviour remains unchanged. A directory fsync can be reconsidered if the hub is moved to Linux.
2. **Review #5 suggested alternative — show the last path segment.** Rejected because Claude's encoded project directory is itself that segment (for example `C--Users-<name>-...`), so this would preserve the privacy leak rather than fix it.

## Test results

- `cargo fmt --all -- --check` — exit 0, no output.
- `cargo build --workspace --offline` — exit 0; workspace build finished successfully.
- `cargo clippy --workspace --all-targets --offline -- -D warnings` — exit 0; no warnings.
- `cargo test -p cctg --offline` — exit 0; all cctg unit, integration, and doc tests passed (61 library tests passed, 1 ignored; all other cctg test binaries passed).
- `cargo test --workspace --offline` — exit 0; **143 passed, 0 failed, 1 ignored** including the compile-fail doctest.
- `git diff --check` — exit 0; only Git's existing Windows LF-to-CRLF notices were printed.
- Privacy scan of the five changed Rust files for `AppData`, `C--Users-user`, and `C:\Users\user` — no matches.

No Telegram calls were made, `.env` and `~/.claude` were not read, and no commit was created.

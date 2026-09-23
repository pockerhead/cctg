# FIX SUMMARY — TASK-008

## Fixed

- **minor-1 — starvation of ordinary messages by edits/topic mutations.** Confirmed in `Scheduler::pick`: ready ordinary messages were considered only after both unmetered lanes. Added bounded fairness after four consecutive unmetered dispatches. The eligible permission check remains first, and ordinary dispatch still takes the head of the shared message queue, preserving permission-first across topics and FIFO within a topic. Added a paused-Tokio test with 200 ms transport latency and edits arriving every 100 ms for 60 virtual seconds; the message queued at t=0 is delivered within 3 seconds.
- **minor-2 — zero-delay retry on `retry_after = 0`.** Confirmed. Bot API decoding now clamps an explicit zero to one second; a missing value retains the existing five-second fallback. The scheduler independently enforces the same one-second minimum so a custom/fake transport cannot create a tight loop. Tests cover decoding and scheduler attempts at t=0 and t=1 s.
- **minor-3 — env-file secrets copied into the process environment.** Confirmed: `dotenvy::from_path` mutates process env. Replaced it with `dotenvy::from_path_iter`, collected file values locally, and resolved configuration with process env taking precedence. Parse/read errors remain redacted. An isolated child-process test verifies both that a file token is absent from `std::env::var` after loading and that an already-set process value wins.
- **nit — nonempty `getUpdates` batch without any integer `update_id` could spin.** Confirmed. Such a stalled batch now gets a one-second backoff; empty successful long-poll batches and advancing batches do not. Added a policy unit test.

## Skipped

- **Unbounded internal `VecDeque` mailboxes.** Left unchanged as explicitly directed. The inbound Tokio channel remains bounded at 1024; changing the internal drain/queue architecture is outside this fix.
- **Random-arrival sliding-window test from “Missing coverage”.** No corresponding product defect was found, and the reviewer’s scratch result already indicated the current bucket behavior was correct. Existing workspace tests still cover the 20/minute window, failed attempts, per-topic FIFO, and 429 timing; scratch scripts were not treated as independent verification.
- **Whole-loop polling test and callback-without-message coverage.** Code inspection confirmed the callback path still applies the allowlist, and the polling loop retains offset/backoff behavior. The concrete no-`update_id` defect received a direct policy test without introducing a network abstraction solely for test coverage.
- **Public `BotApi::chat_id()` with one consumer.** Harmless API-shape nit; no behavioral defect.
- **Historical release build in the repository target directory.** No source defect to fix. All fixer Cargo commands used the repository’s default target directory as required by the orchestration prompt.
- **Suggested unconditional lane alternation.** Not implemented verbatim because it could send an ordinary message before an eligible permission prompt. The bounded-streak implementation fixes starvation while retaining the required priority order.

The pre-existing modification to `maw/tasks/in_progress/TASK-008/metrics.md` was preserved and not edited by the fixer.

## Test results

- `cargo test -p cctg --lib --offline`
  - `31 passed; 0 failed; 1 ignored`.
  - The ignored helper is invoked by the passing parent test in an isolated subprocess.
- `cargo fmt --all -- --check`
  - Exit 0; no output.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`
  - Exit 0; `Finished dev profile`.
- `cargo test --workspace --offline`
  - Exit 0; 107 passed, 0 failed, 1 ignored helper across unit, integration, and doc tests.
  - `routing_logs` remains in its own integration-test binary.
- `cargo build --workspace --offline`
  - Exit 0; `Finished dev profile`.
- `git diff --check`
  - Exit 0; only the repository’s existing LF-to-CRLF warnings.
- `rg -n "set_var|dotenvy::from_path\\(" crates/cctg/src crates/cctg/tests`
  - No matches.

No real Telegram API call was made, no real `.env` was read, and no commit was created.

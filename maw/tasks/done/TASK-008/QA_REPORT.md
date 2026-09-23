# QA REPORT — TASK-008

Stage: qa (claude/opus, effort=medium). Code: `git diff main -- Cargo.toml Cargo.lock crates/` on branch `feature/hub-telegram-foundation` (HEAD `ab3a974`, clean tree). Real Telegram was not called and `.env` was not read. No code files in the repo were changed.

## Disconfirmation (first)

Counter-example I wrote down before testing: "after 4 unmetered jobs the new fairness branch in `pick` sends the head of `message` (an ordinary message) while an eligible permission prompt of another topic is waiting, or it sends a job ahead of an older job of its own topic."

Check: in the code (`scheduler.rs:386-408`) the permission branch runs before the fairness branch, and both need the same `bucket.ready_at`. So when the fairness branch runs, `next_permission()` is `None`, and the head of `message` never has an older job of its own topic. To test this in practice I copied `Scheduler::run` into a step driver in a scratch copy of the workspace. At every dispatch it checks the live queue: (a) an ordinary message never goes while an eligible prompt exists, (b) nothing older of the same topic is queued, (c) the bucket is ready for every metered pick, (d) no unmetered streak ≥4 while a message is ready. I ran it on 120 random scenarios (edits, topic ops, sends, documents, prompts, General topic, 200-900 ms latency, 429 with retry_after 0/2/4/6 s). **The counter-example did not hold.** The harness is sensitive: 3 of 3 injected mutations were caught (fairness before permission, no same-topic guard, capacity 7 giving 22 attempts per 60 s).

## 1. Environment

- No docker-compose or dev server. Only the cargo test runner. No external services (Telegram is replaced by fakes and a loopback TCP server inside the tests).
- `CARGO_TARGET_DIR=%TEMP%/cctg-qa008-target` (outside the repo), one cargo command at a time, `--offline`.
- Independent tests are in a scratch copy of the workspace: `maw/tasks/in_progress/TASK-008/scratch/qa/ws` (snippets `scratch/qa/*.rs.txt` added to the ends of `scheduler.rs`, `api.rs`, `updates.rs`; dev-dep tokio `+net,io-util`).

Reproduce:
```
export CARGO_TARGET_DIR="$TEMP/cctg-qa008-target"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
cd maw/tasks/in_progress/TASK-008/scratch/qa/ws && cargo test -p cctg --lib --offline qa_ -- --nocapture
# flake: run routing_logs-*.exe from $CARGO_TARGET_DIR/debug/deps 1000 times
```

## 2. Test results

Existing suite:
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: clean.
- `cargo test --workspace --offline`, 4 full runs: **107 passed, 0 failed** each time (cctg lib 31 + 1 ignored helper, main 1, routing_logs 1, stdout 3, transcript 70 + 1 doc-test).
- Flake runs: cctg lib binary 90/90 green, stdout 10/10 green, **`routing_logs` failed 9 of ~1640 runs** (3/640 and 6/1000). See BUG-1.

New tests (scratch, not in the repo):

| Test | Result |
|---|---|
| `scheduler::qa_random_mixed_traffic_many_seeds`: 120 scenarios, step-driver invariants plus closed 60 s window ≤20, gap ≥1 s, FIFO per topic over successful sends, no lost replies | PASS (max window 20 in 95 scenarios, never 21) |
| `scheduler::qa_edit_flood_with_permissions_and_messages`: edits every 50 ms, 300 ms latency, 30 sends + 6 prompts | PASS; metered sends at 0, 1.2, 2.4 … 124.2 s, so the bucket rate holds under the flood |
| `api::qa_error_paths_never_render_the_token`: 401, HTML 502, wrong type 200 (Decode), truncated body, non-HTTP answer, redirect to a closed port, redirect loop, invalid URL; checks `{}`, `{:#}`, `{:?}`, `{:#?}` of `anyhow` with context, like `main` prints | PASS: no secret, no bot id |
| `api::qa_retry_after_zero_over_the_wire_is_one_second`: real HTTP 429 with `retry_after: 0` | PASS (1 s) |
| `updates::qa_callback_without_message_is_gated_by_allowlist`, `qa_inaccessible_callback_message_does_not_break_parsing`, `qa_service_message_in_other_chat_is_other_chat`, `qa_general_topic_reply_has_no_thread`, `qa_null_fields_are_tolerated` | PASS |
| `updates::qa_null_and_huge_fields` (`update_id = i64::MAX`) | **FAIL**: `attempt to add with overflow` at `updates.rs:152`. See BUG-2 |
| CLI config checks with the built `cctg.exe` (no network: every case fails at config): process env beats file (`entry #2` vs `#1`), default `./.env` is read, bad token/chat/allowlist, unterminated quote, bad `${`, invalid UTF-8, directory as env file | PASS: stderr names the variable or the file only, never the secret or the id |
| `grep` of non-test code for `set_var`, `from_path(`, `println!`, `dbg!`, `unwrap`, `expect`, `token.expose` | clean; `expose()` is used only to build `BotApi.base` |

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| 1. Sender outside the allowlist does not reach handlers; neither `from.id` nor the token reaches logs on any error path | Existing allowlist tests plus my callback-without-message and inaccessible-message cases. `Inbound`/`CallbackInput` have no user id. My loopback error matrix (8 error kinds) rendered through anyhow with context. CLI config errors. grep for logging of secrets. `reqwest`/`hyper-util` logs at INFO and above do not include URLs, and the subscriber is INFO with no env-filter | PASS |
| 2. 20 messages/min per group with fake time; order inside a topic kept | Existing tests plus 120 random scenarios with 429s: window ≤20 (closed interval), gap ≥1 s, per-topic FIFO | PASS |
| 3. Repeated edits coalesce; 429 retried no earlier than `retry_after`, no retry storm | Existing tests (`repeated_edits_of_one_message_coalesce`, `repeated_429_is_one_attempt_per_retry_after`, `zero_retry_after_still_pauses_for_one_second`). The 1 s floor is checked in `parse_envelope` (`api.rs:366-369`) and in `dispatch` (`scheduler.rs:427`), and over real HTTP | PASS |
| 4. Topic creation/edit go through a separate lane, do not spend message tokens; no hardcoded numeric mutation limit | `metered()` only covers `Send`/`SendDocument`. The only new constant, `MAX_CONSECUTIVE_UNMETERED = 4`, is a fairness streak for messages, not a mutation rate limit: unmetered ops are never delayed by time. `topic_mutations_and_edits_do_not_spend_message_tokens` passes | PASS |
| 5. Service messages `forum_topic_created/edited/closed/reopened` are recognised and not routed as input | Existing test (bot and allowlisted sender) plus other-chat service message → `OtherChat` | PASS |
| 6. Unknown fields / unknown update type do not stop polling | Existing batch test plus null fields. Edge case: `update_id = i64::MAX` panics in a debug build (BUG-2, not a real Telegram value) | PASS (with low note) |
| 7. Missing `can_manage_topics` found at startup with a clear error | `check_topic_rights` matrix. `run` order getMe → getChatMember → check before scheduler/poll (`mod.rs:43-57`). Not checked live (would need the real bot) | PASS (code + unit) |
| 8. Binary size / build time / idle RSS recorded in the task notes | IMPL_SUMMARY §3 has all three with evidence files (they exist and match). My post-fix release build: **5,491,200 B**, 43.6 s (fresh release profile in a new target dir) | PASS |
| 9. Existing tests pass | 107/107 in 4 full runs, but `routing_logs` flakes ~0.5% | **FAIL (flaky)**, see BUG-1 |

Fixer claims checked against the code:
- Lane fairness: bounded streak, prompt still first, head-of-queue keeps FIFO, ≤20 per 60 s. Confirmed by the harness.
- `retry_after` floor: `max(1 s)` both in the API and in the scheduler. Confirmed.
- Config without `set_var`: `dotenvy::from_path_iter` into a map, `std::env::var(name).ok().or_else(file)`. Process env wins (checked on the CLI), and errors carry only a fixed `reason`. Confirmed. Two side effects: a duplicate key in the file now resolves to the last value (the old `from_path` kept the first), and a set-but-empty process variable hides the file value. Both are harmless.
- Stalled batch backoff: `stalled_batch_backoff` is covered by a unit test. Confirmed.

## 4. Bugs found

### BUG-1 (medium, test-only): `tests/routing_logs.rs` is flaky, ~0.5%
- Reproduce: build tests, then run `routing_logs-*.exe -q` 1000 times. I got 6/1000 and 3/640 failures.
- Expected: always passes (nothing leaks). Actual: `user id in logs:` panic. The captured log does not contain an id. The fmt subscriber timestamp contains the digits, e.g. `2026-09-23T01:02:03.331001Z` contains `1001` (ALLOWED), and `…05.620028Z` contains `2002` (STRANGER). The assertion is a plain substring search over logs with timestamps.
- Impact: a false failure in "Existing tests pass" on roughly every 200th CI run. This is the same test the pipeline already isolated once for a different flake.
- Fix: add `.without_time()` to the subscriber in `routing_logs.rs` (and/or use ids that cannot appear in a timestamp, like the `std::process::id()`-based markers used in other tests).

### BUG-2 (low): `update_id` overflow panics `route_batch` in debug builds
- `updates.rs:152`: `id + 1` with `id = i64::MAX` gives `attempt to add with overflow` (debug). In release it wraps to `i64::MIN`, and the negative offset makes `getUpdates` return recent updates again. Telegram ids are far from `i64::MAX`, so this is not realistic, but it is unchecked arithmetic on external input (project law: no panics on external input). Fix: `id.checked_add(1)` / `saturating_add(1)`.

### Notes (no fix required)
- Edits and topic mutations can still go out before an older `Send` of the same topic (known, documented in PLAN_FINAL §5).
- `IMPL_SUMMARY` measurements were taken before the fixer's changes. My post-fix size is 5,491,200 B (+512 B).
- Not executed: a live run against the real bot (startup rights check, idle RSS under long poll), because the task forbids real Telegram calls. Deferred to TASK-009 by the orchestrator.

## 5. Verdict

**NEEDS_FIXES.** The product code meets criteria 1-8. I found no path where the token or a user id reaches logs, errors or panics, and the fixer's scheduler changes hold under adversarial random traffic. But the delivered test suite has a reproducible ~0.5% flake (BUG-1), so "Existing tests pass" is not reliable. The fix is one line (`.without_time()`). BUG-2 is a one-line `saturating_add` and can go in the same small fix.

## Cleanup

No services or containers were started. Build output is only in `%TEMP%/cctg-qa008-target` (outside the repo, can be removed). Scratch evidence is in `maw/tasks/in_progress/TASK-008/scratch/qa/`.

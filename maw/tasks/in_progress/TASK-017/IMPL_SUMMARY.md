# TASK-017 IMPL_SUMMARY

Verdict: IMPLEMENTED (plan step 0, mechanical path)

## Pre-flight
- `git apply --check scratch/reviewer2/task017.patch` on HEAD `4cececb` (branch `feature/dead-slot-buffer`, clean tree): OK. Between the plan base and HEAD only task artifacts changed.
- Spot checks of named entities before applying: `Slot.pending_separator` (registry.rs:275), `Registry::slot` (527), `HookEvent::SessionEnd` branch with `entry.waiting = false;` (850/863), `OFFLINE_NOTICE` and `live_agent` in slots.rs, `permissions::MAX_CALLBACK_DATA = 64`. All match the plan.

## 1. What was implemented
Patch applied as-is; `scratch/reviewer2/verify_hashes.sh`: 7/7 OK (`scratch/implementer/hashes_after_apply.txt`).

| File | Change |
|---|---|
| crates/cctg/src/hub/buffer.rs | new, 250 lines |
| crates/cctg/src/hub/mod.rs | +1 |
| crates/cctg/src/hub/registry.rs | +54 (incl. `entry.agent = None;` in the SessionEnd branch, line 878) |
| crates/cctg/src/hub/slots.rs | +785 / -104 (git: 889 lines touched) |
| crates/cctg/tests/buffer_e2e.rs | new, 229 lines |
| crates/cctg/tests/message_logs.rs | 32 lines touched |
| crates/cctg/tests/overflow_logs.rs | 6 lines touched |

## 2. Not implemented / deviations
None. No file outside the patch was touched.

## 3. Test results
One `CARGO_TARGET_DIR=%TEMP%/cctg-t017-impl`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time; dir deleted afterwards.
- `cargo fmt --all --check`: exit 0.
- `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings`: exit 0 (`scratch/implementer/clippy.txt`).
- `cargo test -j 1 --offline --workspace --no-fail-fast`: exit 0 (`scratch/implementer/workspace_test.txt`). cctg lib 390 passed, 1 ignored; buffer_e2e 1, message_logs 1, overflow_logs 1, stream_e2e 11, all other binaries ok. No flakes in this run.

## 4. Manual verification
- `cargo test -p cctg --lib hub::buffer` and `hub::slots` (tests named in plan section 5), `cargo test -p cctg --test buffer_e2e`.
- Live (not run here, needs Telegram): end a session in a slot, write 3 messages in its topic: one message with the "Возобновить" button appears, the topic stays open with the dead icon; pressing the button answers that launch from Telegram is not connected yet; `claude --resume <id>` with the channel flag delivers the 3 messages in order and the button message turns into plain text. `registry.json` holds the texts under `slots[..].buffer` while waiting, never a user id.

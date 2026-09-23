# IMPL_SUMMARY — TASK-021

**Verdict: IMPLEMENTED**

## Pre-flight

- `git diff 961daf8 HEAD -- crates docs Cargo.toml Cargo.lock` is empty: the source matches the base the patch was built on.
- `git apply --check scratch/reviewer2/task021.patch` passed.
- Shape check of the entities the patch touches: `slots::Control` is `#[derive(.., Copy, ..)]` (slots.rs:61), `Conn._to_agent` exists (slots.rs:173), `transcript::split_for_telegram(text, SplitOptions) -> SplitResult` with `prefer_file` (crates/transcript/src/split.rs:38), `wire::AgentMsg::Reply` and `wire::HubMsg::Inbound` exist (wire.rs:142, 176). All as the plan assumes.

## 1. What was implemented

Plan Step 0: `git apply maw/tasks/in_progress/TASK-021/scratch/reviewer2/task021.patch`, then `bash scratch/reviewer2/verify_hashes.sh`: all 9 files `OK`.

| File | Change |
|---|---|
| crates/cctg/src/hub/api.rs | +10 (`Message.reply_to_message`, `MessageRef`) |
| crates/cctg/src/hub/updates.rs | +44 / -4 (`Inbound.reply_to`, explicit-reply filter, test) |
| crates/cctg/src/hub/commands.rs | +1 (`reply_to: None` in test literal) |
| crates/cctg/src/hub/mod.rs | +53 / -4 (`route_inbound` -> `Control::Message`, `chat_id` in `Options`, test) |
| crates/cctg/src/hub/slots.rs | +604 / -11 (routing, reply, notices, backlog cap, tests) |
| crates/cctg/tests/command_logs.rs | +1 |
| crates/cctg/tests/message_logs.rs | new, 219 lines |
| crates/cctg/tests/overflow_logs.rs | new, 182 lines |
| docs/poc.md | new, 94 lines |

Tracked diff total: 6 files, 713 insertions, 19 deletions.

## 2. Deviations

None. Patch applied byte-for-byte, no hand edits.

## 3. Test results

Build env: default repo `target/` (per orchestrator note), `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time.

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: exit 0, 20 test binaries `ok`; cctg lib 233 passed, 1 ignored; `message_logs` 1 passed; `overflow_logs` 1 passed. Matches the plan expectation. Output (stdout): `scratch/impl_workspace_test.txt`.

Mutation runs were not repeated (the plan records them in `scratch/reviewer2/mutations.out.txt`).

## 4. Manual verification

- Unit/integration: `cargo test -p cctg --lib hub::slots` and `cargo test -p cctg --test message_logs --test overflow_logs`.
- Live: `docs/poc.md` (needs the real bot and a claude login in the isolated `CLAUDE_CONFIG_DIR`; for the orchestrator/user after merge). Checklist there: text in the slot topic reaches Claude, the `reply` tool answer lands in the same topic, a message to a dead slot gets one "не на связи" notice, `/brief` still goes to the command worker.

# IMPL_SUMMARY — TASK-013 (implementer, claude/opus)

**Verdict: IMPLEMENTED** (plan followed as written, no deviations)

## Pre-flight

- `git status` clean on `feature/agent-channel` (HEAD `af0b3e1`); `crates/`, `Cargo.*`, `docs/` unchanged since `767c334` (`git diff 767c334 HEAD --stat` empty).
- `scratch/reviewer2/task013.patch` sha256 = `d6b9450664cee66558ceb1d51b2d58fa813cfd0e97101b0e2225d5df47592ce9` (matches plan).
- `git apply --check` passed. Named entities exist with the expected shape: `device::canonical_cwd(&str) -> String` (device.rs:147), `proctree::current_lineage` (proctree.rs:95), `wire::Register` (wire.rs:113), `wire::VERSION = 1`, `agent::spawn(LinkConfig)` (agent.rs:81).

## 1. What was implemented

Step 1: `git apply scratch/reviewer2/task013.patch`, then `verify_hashes.sh` -> 13 x OK, exit 0. Nothing written by hand.

| File | +/- |
|---|---|
| crates/cctg/Cargo.toml | +1 -1 (tokio `io-std`) |
| crates/cctg/src/agent.rs | +444 -6 |
| crates/cctg/src/channel.rs (new) | 942 |
| crates/cctg/src/device.rs | +13 -2 |
| crates/cctg/src/hub/ingress.rs | +1 (test literal) |
| crates/cctg/src/hub/registry.rs | +49 -3 |
| crates/cctg/src/hub/slots.rs | +230 -9 |
| crates/cctg/src/lib.rs | +1 |
| crates/cctg/src/main.rs | +31 -5 |
| crates/cctg/src/wire.rs | +20 |
| crates/cctg/tests/agent_stdio.rs (new) | 185 |
| crates/cctg/tests/ingress_logs.rs | +1 |
| crates/cctg/tests/slots_logs.rs | +1 |

Exactly the 13 files from `hashes.txt`; `Cargo.lock` unchanged.

## 2. Not implemented / deviations

None. Built with the default repo `target/` per orchestrator override (not `%TEMP%`). Live checks (plan section 3, "for QA") not run: plan marks them optional for the implementer, and they need a temporary `--mcp-config` run of Claude Code.

## 3. Test results

Outputs saved in `scratch/implementer/`.

- `cargo test --workspace --offline -j 1` -> exit 0, **311 passed, 0 failed, 1 ignored** (matches plan expectation). `scratch/implementer/cargo_test.txt`.
- `cargo clippy -p cctg --all-targets --offline -j 1 -- -D warnings` -> exit 0. `scratch/implementer/clippy.txt`.
- `cargo fmt --all -- --check` -> exit 0.
- `git diff --check` -> exit 0.

## 4. Manual verification

- `cctg agent-install` prints `claude mcp add --scope user cctg -- "<abs exe>" agent` (does not execute it).
- Pipe a script into `cctg agent` with a clean env (no secret): `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}`, `{"jsonrpc":"2.0","method":"notifications/initialized"}`, `{"jsonrpc":"2.0","id":2,"method":"tools/list"}`, `{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"reply","arguments":{"text":"hi"}}}`, `{"jsonrpc":"2.0","id":4,"method":"nope"}`: stdout has one JSON object per line, id 4 is `-32601`, reply is `isError` (no hub), a single warn line appears on stderr only.
- Live channel with a mock hub: recipe in PLAN.md section 4 (`scratch/planner/fake_hub.py`, temporary `--mcp-config`, `--dangerously-load-development-channels server:cctg`); check `Channel notifications registered` in the debug log, inbound delivery, `reply` reaching the mock hub, and that `/clear` keeps one agent pid while the hub rebinds to the new session.

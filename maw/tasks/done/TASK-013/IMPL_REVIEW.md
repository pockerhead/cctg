# Verdict

**NEEDS_WORK** - MCP/agent and `/clear` rebinding are implemented as planned, but the bounded permission tracker can forget a request that was already relayed, causing its valid verdict to be dropped and a duplicate to be relayed again.

## Disconfirmation

The concrete counterexample tested first was `/clear` with reversed hook order: `SessionStart(B, pid=P)` arrives before the late `SessionEnd(A, pid=P)`, while the surviving agent reconnects with stale env session id `A`. The counterexample did **not** hold. `Registry::apply_hook` removes the pid only when it still points to the ending session, `Slots::follow_pid` moves the live connection to `B`, and `Slots::agent_session` resolves a stale reconnect through the live `(host, pid)` mapping. The exact order is exercised by `the_agent_follows_its_claude_process_when_the_new_start_comes_first` (`crates/cctg/src/hub/slots.rs:954`).

## Confirmed correct

- The JSON-RPC surface is hand-written and keeps stdout ownership in one writer. It validates `jsonrpc: "2.0"`, ids and structured params; implements version negotiation, `ping {}`, `tools/list`, `tools/call`; returns controlled parse/invalid-request/method errors; and queues inbound notifications until `notifications/initialized` (`crates/cctg/src/channel.rs:103`, `crates/cctg/src/channel.rs:126`, `crates/cctg/src/channel.rs:170`, `crates/cctg/src/channel.rs:277`).
- `reply` returns MCP tool results with `isError`, distinguishes unavailable/full hub states, and caps worst-case escaped wire payloads below `wire::MAX_LINE`. Permission free-text fields are capped similarly (`crates/cctg/src/channel.rs:325`, `crates/cctg/src/channel.rs:367`, `crates/cctg/src/channel.rs:906`).
- Inbound meta keys are filtered by `[A-Za-z0-9_]+`; retained string values are passed through unchanged (`crates/cctg/src/channel.rs:385`).
- The stdio reader bounds and discards oversized lines, hub I/O runs independently, reconnects with backoff and re-registers, and a failing hub does not stop MCP handling (`crates/cctg/src/agent.rs:99`, `crates/cctg/src/agent.rs:248`, `crates/cctg/src/agent.rs:317`, `crates/cctg/src/agent.rs:376`).
- Headless `sdk-cli`, missing session id and missing/bad device configuration all continue serving MCP without opening a hub link (`crates/cctg/src/agent.rs:285`, `crates/cctg/tests/agent_stdio.rs:162`). Interactive sessions without the development-channel flag remain intentionally indistinguishable to the agent, consistent with `OPEN_DECISIONS.md`.
- Registration uses the shared `DeviceConfig.host` and `device::canonical_cwd` helpers and derives the owning Claude pid from the process tree rather than inherited `CLAUDE_PID` (`crates/cctg/src/agent.rs:260`, `crates/cctg/src/device.rs:54`, `crates/cctg/src/device.rs:147`). `Register.claude_pid` is optional without changing wire version 1 (`crates/cctg/src/wire.rs:113`).
- Hub binding rejects nested sessions and follows `(host, claude_pid)` through `/clear` in both hook orders; stale disconnects do not clear the new binding (`crates/cctg/src/hub/registry.rs:764`, `crates/cctg/src/hub/registry.rs:777`, `crates/cctg/src/hub/slots.rs:315`, `crates/cctg/src/hub/slots.rs:332`).
- Agent tracing and the panic hook write fixed/sanitized text to stderr; the process deliberately exits 0 after the agent task ends. Integration coverage triggers a real failing-hub log and verifies JSON-only stdout and secret-free stderr (`crates/cctg/src/main.rs:55`, `crates/cctg/tests/agent_stdio.rs:111`).
- The dependency change only enables Tokio's existing `io-std` feature. `Cargo.lock` is unchanged and no MCP crate, `rmcp`, or `teloxide` was added (`crates/cctg/Cargo.toml:12`).
- The two relevant `dead_end` entries were checked against primary artifacts: the current diff is limited to the expected 13 files and is whitespace-clean; the corrected reverse-order `/clear` test targets the reused topic (thread 100), not a spurious second topic.

Independent verification used an external `CARGO_TARGET_DIR`, `CARGO_PROFILE_DEV_DEBUG=0`, offline mode and `-j 1`: `cargo test --workspace` passed with **311 passed, 0 failed, 1 ignored**; `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` all passed.

## Issues

### Major - an already-relayed permission request can be forgotten

- **File:** `crates/cctg/src/channel.rs:271`
- **Description:** After successfully relaying a 65th distinct permission request, the code evicts the oldest id from `open_permissions`. That oldest request is still open in Claude Code and already visible in the hub/Telegram. Its later legitimate verdict is now discarded as "not open"; if Claude repeats the same request id, it is relayed a second time. This breaks the stated first-answer/open-id semantics and the duplicate suppression guarantee on a security-sensitive path.
- **Suggested fix:** Never evict an id after its request has been relayed. Check capacity before `try_send` and decline the new relay while retaining every already-relayed open id, or track all relayed ids until verdict/cancellation with a safely bounded expiry policy. Add a test that opens 64 ids, submits a 65th, then proves the oldest verdict is still emitted exactly once and a duplicate oldest id is not re-relayed.

## Missing coverage

- The `MAX_OPEN_PERMISSIONS` boundary and overflow behavior described above.
- A deliberate panic-path process test proving fixed stderr, empty/non-protocol stdout, and exit code 0. The implementation is structurally correct, but the current stdout integration test does not trigger a panic.
- A real unreachable-listener case (not only an accept-and-immediately-close hub) while MCP requests continue, plus a hub rejection/version-mismatch case.
- Shell-special characters in the executable path printed by `agent-install`. The current test covers spaces only; POSIX double quotes do not neutralize `$`, backticks, or embedded quotes.

## Nits

- `install_command` builds a shell command with ad-hoc double quoting (`crates/cctg/src/agent.rs:435`). This is adequate for ordinary Windows paths, but a platform-aware quoting helper or an argv-style instruction would make the printed command safe on supported POSIX hosts as well.

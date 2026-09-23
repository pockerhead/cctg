# FIX_SUMMARY — TASK-013

## Fixed

- **Major — already-relayed permission ids were evicted at capacity.** `channel::Server` now checks `MAX_OPEN_PERMISSIONS` before relaying a new distinct request. It never evicts an open id; the new request remains in Claude Code's local terminal dialog. A fixed warning is emitted once for an overflow burst and is re-armed after a verdict frees capacity. The boundary test opens 64 ids, rejects the 65th relay, proves the oldest verdict is emitted exactly once, proves a repeated oldest id is not re-relayed, and proves the rejected id can be relayed after capacity becomes available.
- **Missing coverage — panic handling.** The agent panic hook now calls `write_panic_message`, which accepts only a writer and prints the fixed `cctg agent: internal error` line without receiving the panic payload. Its direct test proves the exact stderr bytes and that no stdout buffer is touched. `main` still ignores the spawned task's panic result and exits with code 0.
- **Missing coverage — unreachable hub.** Added a process test using a loopback port with no listener. The real `cctg agent` continues returning valid JSON-RPC responses and exits successfully when stdin closes while the connect attempt is pending or failing.
- **Missing coverage — hub rejection/version mismatch.** Added a process test with a mock hub that reads hello/register and returns `rejected { reason: version }`. MCP stdio remains usable, stdout remains JSON-RPC-only, and neither output stream contains the shared secret.
- **Nit — `agent-install` shell quoting.** Windows retains double quotes. POSIX now uses single quotes and escapes embedded single quotes as `'\''`. A POSIX-only test covers `$`, backticks, and embedded quotes.

## Skipped

- **Review alternative: bounded expiry/TTL for relayed permission ids.** Skipped because it would recreate the major bug for a delayed legitimate verdict. A relayed id is retained until its verdict or process/session end, as required by the orchestrator decision.
- **Synthetic process-only panic switch.** Not added: integration-test binaries compile the production binary without `cfg(test)`, and a hidden release environment switch would add a production panic trigger. The orchestrator explicitly allowed testing the panic hook function directly when no cheap real production panic is reachable; that safer path was used.

## Test results

- `cargo test -p cctg channel::tests::permission_capacity_never_forgets_an_open_relayed_id --offline -j 1` — `1 passed; 0 failed`.
- `cargo test -p cctg --test agent_stdio --offline -j 1` — `4 passed; 0 failed`, including unreachable-listener and version-rejection process tests.
- `cargo test -p cctg agent::tests::panic_message_is_fixed_stderr_only --offline -j 1` — `1 passed; 0 failed`.
- `cargo test --workspace --offline -j 1` — `315 passed; 0 failed; 1 ignored`.
- `cargo clippy --workspace --all-targets --offline -j 1 -- -D warnings` — passed with no warnings.
- `cargo fmt --all -- --check` — passed.
- `cargo build --workspace --offline -j 1` — passed.
- `git diff --check` — passed (only Git's existing LF-to-CRLF checkout notices were printed).

## Round 2 (QA_REPORT: NEEDS_FIXES)

Preflight claim checked first: QA's first suggestion, "bring back bounded FIFO/LRU eviction of the oldest id", taken verbatim with the verdict filter still in place (`on_link` dropped any verdict whose id was not in `open_permissions`) would restore the round-1 bug: a late Telegram verdict for an evicted but still-pending prompt would be dropped. Checked in `crates/cctg/src/channel.rs` (the old `PermissionVerdict` arm). That is why eviction and the verdict filter change together, per the orchestrator decision.

### Fixed

- **Bug 1 (major), relay dies after 64 prompts answered in the terminal.** Confirmed in code: `open_permissions` was cleared only by a hub verdict, and at 64 entries new requests were refused. Fix in `channel.rs`: `open_permissions` + `permission_overflow_warned` replaced by `recent_permissions`, a FIFO of `RECENT_PERMISSIONS = 256` recently relayed ids. The oldest id is evicted when the window is full. A repeated id inside the window is not relayed again. The capacity refusal and its warning are gone. Every hub verdict with a well-formed id (`is_request_id`) goes to Claude Code, whether or not it is in the window; a malformed id is dropped. A verdict also removes its id from the window, so a later request with the same id is relayed again (decision logged in `log.jsonl`). Tests:
  - `prompts_answered_in_the_terminal_never_stop_the_relay`: the `perm_leak.py` scenario in Rust, 326 distinct requests (more than 64 and more than the window), no verdicts, all relayed in order.
  - `a_recent_duplicate_is_suppressed_and_an_evicted_verdict_still_passes`: a duplicate inside the window is not relayed; once the id is evicted its verdict is forwarded as exactly one notification; after that the same id is relayed as a new request.
  - `a_verdict_is_forwarded_without_a_relayed_request_but_never_malformed`.
  - Changed `a_duplicate_permission_request_is_relayed_once` (was `..._and_closed_once`) and `permission_relay_round_trip`: a second verdict or a verdict for an unknown id is no longer dropped by design; the stray-verdict case now uses a malformed id. Removed `permission_capacity_never_forgets_an_open_relayed_id` (it pinned the behaviour QA showed was wrong).
- **Advice, deferred `reply` tool.** `INSTRUCTIONS` now names the tool by its full name `mcp__<server>__reply` (server name as registered, normally `mcp__cctg__reply`) and says it may be deferred and loaded with ToolSearch. `initialize_declares_the_channel` asserts these phrases.
- **Nit, `agent-install` on Windows.** `quote_shell_arg` on Windows now prints a PowerShell literal: single quotes, `'` doubled. POSIX single-quote escaping is unchanged. The old all-platform test (it expected Windows double quotes and would have failed on POSIX) is now `#[cfg(windows)] install_command_quotes_the_absolute_path_for_powershell` and also covers `'`, `$` and a backtick. Doc comment of `install_command` says which shell it is quoted for. Real output: `claude mcp add --scope user cctg -- 'C:\Users\user\dev\cctg\target\debug\cctg.exe' agent`.

### Skipped

- Nothing from the QA report. Not done on purpose: hub-side deduplication of repeated verdicts (Claude Code ignores a verdict for an id that is not pending, and hub buttons are TASK-014).

### Test results

- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: exit 0, no warnings.
- `cargo test --workspace --offline`: all green, 317 passed, 0 failed, 1 ignored (lib 222 passed / 1 ignored; `agent_stdio` 4 passed).
- `git diff --check`: clean.
- `python scratch/qa/perm_leak.py <repo>/target/debug/cctg.exe 330` on the real binary (output in `scratch/fixer/perm_leak_330.out.txt`): `relayed to hub: 330`, `not relayed: []`, exit code 0, stdout 1 line of valid JSON, secret in stdout/stderr `False False`. The `ConnectionResetError` in that file comes from the script's own hub thread when the agent exits, not from the agent.

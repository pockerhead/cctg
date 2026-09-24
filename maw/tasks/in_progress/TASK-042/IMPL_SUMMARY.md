# TASK-042 implementer summary

## Root cause (confirmed)

1. Hub side. `AgentEvent::Disconnected` called `Registry::agent_disconnected`, which set `sessions[s].agent = None` when the closing conn was the bound one. A second `Registered` of the same session takes the binding (TASK-021 rule), so when that newer conn closed, the session had no agent although the first conn was still open. Reproduced by the new test `a_short_lived_second_agent_leaves_the_session_bound_to_the_first`: with the rebind call disabled it fails (icon goes to 👀 no channel); with the fix it passes.

2. Source of conn 8. `crates/cctg/tests/stdout.rs::subcommands_do_not_write_to_stdout` ran `cctg agent` with `Command::output()` and the full inherited environment: `CLAUDE_CODE_SESSION_ID` of the Claude Code session that ran `cargo test`, and the real `USERPROFILE`, so the real `~/.cctg/device.env` (secret, live hub address). `output()` gives the child a null stdin, so the agent registered and exited at EOF within milliseconds. Evidence: in session 1f2c01a2 the subagent `agent-a37d66ade6e7e109d` started `cargo test --workspace` (TASK-040 worktree, same `stdout.rs`) at 18:45:21Z, finished 18:47:33Z, which covers 18:46:41Z. The control in `tests/isolation.rs` reproduces the mechanism: an agent with inherited session id + secret + address connects to the hub address.

## What was implemented

- `crates/cctg/src/hub/slots.rs` (+106/-3): on `Disconnected` of the bound conn, `Slots::rebind` binds the session again to `Slots::heir`, the newest still open conn of the same session whose `claude_pid` equals the session entry's pid (any conn when the entry pid is unknown), only for a live top-level session; then re-pushes selected verdicts and resyncs the waiting flag like a fresh registration. A pending (pre-SessionStart) conn that closes hands `pending` to the newest remaining conn of that session. After `SessionEnd` the binding is already `None`, so an old run's link is never re-adopted.
  Tests: `a_short_lived_second_agent_leaves_the_session_bound_to_the_first` (icon never 👀, inbound reaches conn 1, a reply from conn 1 reaches the topic), `a_link_of_another_claude_process_does_not_inherit_the_session` (a conn with another claude pid is not taken; icon 👀).
- `crates/cctg/tests/common/mod.rs` (new, 34 lines): `cctg(home)` and `isolate(&mut Command, home)` drop every `CCTG_*` and `CLAUDE*` variable of the test process and set `USERPROFILE`/`HOME`.
- `crates/cctg/tests/isolation.rs` (new, 125 lines): guard. (a) static scan: every test file that starts cctg uses `common::cctg(`/`common::isolate(`, no `Command::new(env!("CARGO_BIN_EXE_cctg"))` outside the helper; (b) an inner test process gets a session id, secret and hub address (a local listener) in its own env; the helper's agent never connects and logs "no Claude Code session id", a plain agent (control) connects.
- All spawn sites moved to the helper: `agent_stdio.rs`, `hook_cli.rs` (4 sites; the no-home test removes HOME after the helper), `permission_hook_e2e.rs`, `reap_e2e.rs`, `soak.rs` (launcher, so the stand-in and its `cctg hook`/`agent` children inherit an isolated env), `spool_e2e.rs`, `statusline_cli.rs`, `stdout.rs` (agent/hook now in their own temp home; hub runs too), `stream_e2e.rs`, `supervise_e2e.rs` (`clean_command` delegates to `isolate`).

## Not implemented / deviations

- No runtime guard in the `cctg agent` binary itself ("agent without explicit config must not take a foreign session id in tests"): the binary cannot tell a test from a real Claude Code spawn without new env/flags; the tests now never hand it a foreign id. Registration of a second conn still takes the binding (TASK-021 rule kept); only the fallback on disconnect changed.

## Test results

Build: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`.
- `cargo fmt --all --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --workspace` (log `scratch/test_workspace.log`): lib 481 passed; every integration binary ok except `supervise_e2e`, which ran the sibling worktree's `cctg.exe` from the shared target (`--trial-secs` unknown). After `touch crates/cctg/src/main.rs`: `supervise_e2e: ok`, transcript and doc tests ok. An earlier run had the same effect on `spool_e2e` (`client_version` field of the other branch); it passes on rerun (4/4).
- `cargo test -j 1 -p cctg --test soak -- --ignored` (log `scratch/soak.log`): `soak: ok`.

## Manual verification

1. `cargo test -p cctg --lib -- second_agent another_claude_process`.
2. `cargo test -p cctg --test isolation` (from inside a Claude Code session too: the control proves an unisolated agent would connect).
3. Live: with the hub running, start a second `cctg agent` with a live session's `CLAUDE_CODE_SESSION_ID` and stdin closed; the topic keeps ⚡️ and a message in the topic still reaches the session (hub log: "agent bound again after a newer link of its session closed").

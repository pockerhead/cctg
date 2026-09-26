# PCTX proposals (TASK-058)

## 2026-09-26, implementer: hub domain, wire rule

A new agent-to-hub message can be gated by a hub-to-agent message instead of a `Registered` field: the agent announces `Register.status_lines`, the hub then sends `bound{session_id}` (after registration and after `follow_pid`), and the agent sends `status_line` only after a `bound` on that connection. The one message both proves the hub takes `status_line` and tells the agent its current session (the env `CLAUDE_CODE_SESSION_ID` goes stale after `/clear`). Worth recording next to the TASK-014 wire-compatibility lesson.

## 2026-09-26, implementer: universal, shared cargo target

With the shared `CARGO_TARGET_DIR`, another tree building at the same time overwrites `target/debug/cctg.exe` (and test exes) mid-run: tests that start the binary then run a different commit (seen: `statusline_cli` asserting an old line format, `update_e2e` reading a foreign build id, `statusline_agent_e2e` seeing `status_lines: false`), or the link fails with LNK1104 / "failed to remove file". Rule: run the full suite with `--no-fail-fast`, then rerun only the failing targets right after a `touch` of `lib.rs`/`main.rs`; a failure that mentions another commit's build id or output format is contention, not a regression.

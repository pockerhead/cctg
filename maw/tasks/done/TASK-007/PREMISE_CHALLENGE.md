## Counter-example tested

The task is incomplete if the parent transcript contains no subagent sidechain records at all and the current hub has no input or state for a stopped subagent: in that case a pure transcript parser can satisfy the collapsed-block fixture criteria while the real parent rendering can never receive or display that block.

## Primary-source investigation

- The executable currently has no implemented hub path: every `Hub`, `Agent`, and `Hook` command reaches the same empty match arm at `crates/cctg/src/main.rs:27-30`, and the binary's dependency list has no dependency on `transcript` at `crates/cctg/Cargo.toml:6-11`.
- The current pure API parses every sidechain record into an ordinary `Vec<Turn>` at `crates/transcript/src/lib.rs:127-132`; `is_sidechain` is only copied onto each turn at `crates/transcript/src/lib.rs:178-184`. The renderer then iterates all supplied turns directly at `crates/transcript/src/render.rs:53-61` and has no collapsed-subagent model.
- The existing sidechain fixture proves the observable gap: `crates/transcript/tests/parse_fixtures.rs:180-191` expects its user and assistant records as two top-level turns, while `crates/transcript/tests/render.rs:166-170` expects the ordinary output `> List the modules of the crate.\nModules: lib, parse.` rather than one `↳ <type> <id>` block.
- A real metadata artifact, `C:/Users/user/.claude/projects/C--Users-user-dev-cctg/1f2c01a2-63e9-464d-a70f-4a4283d3cd8b/subagents/agent-a002fa7c0795b6f5a.meta.json:1`, contains both `agentType` and `description`, so the optional-meta input assumed by the task exists in the actual system. A real sidechain transcript at `C:/Users/user/.claude/projects/C--Users-user-dev-cctg/d8c72910-ea17-4d68-9c75-57e097fafeea/subagents/agent-a8c1bff86acd31609.jsonl:1,19` contains the subagent prompt and its final `end_turn` text separately, matching the need for a dedicated collapsed representation.
- I ran `cargo test --workspace` from `C:/Users/user/dev/cctg`. Its real output ended with all suites passing: cctg `1 + 1`, transcript parse fixtures `10`, tolerance `15`, purity `3`, render `14`, split `14`, with `0 failed` in every suite.

## Did it hold

The concrete observation was true but did not falsify the premise. The hub is presently only a command-shell placeholder, so there is no existing end-to-end parent renderer for this task to regress or repair. The claimed feature gap is nevertheless present exactly at the pure-library boundary named by the task: real sidechain and metadata inputs exist, while the current API exposes sidechain records only as ordinary turns and cannot represent the requested collapsed block. The passing workspace run also confirms that this is a new feature gap rather than a misdiagnosed existing test failure.

## Verdict

PREMISE HOLDS — `crates/transcript/tests/parse_fixtures.rs:180-191` and `crates/transcript/tests/render.rs:166-170` show the real sidechain fixture is currently exposed and rendered as ordinary top-level turns, while the raw metadata and sidechain artifacts at `C:/Users/user/.claude/projects/C--Users-user-dev-cctg/1f2c01a2-63e9-464d-a70f-4a4283d3cd8b/subagents/agent-a002fa7c0795b6f5a.meta.json:1` and `C:/Users/user/.claude/projects/C--Users-user-dev-cctg/d8c72910-ea17-4d68-9c75-57e097fafeea/subagents/agent-a8c1bff86acd31609.jsonl:1,19` confirm the task's assumed inputs exist.

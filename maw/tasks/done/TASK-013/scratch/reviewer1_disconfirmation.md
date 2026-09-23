# Plan Reviewer 1 — disconfirmation test

Concrete counter-example tested before general evaluation:

If the reference implementation answers the standard MCP `ping` request with JSON-RPC `-32601` instead of an empty `result`, the reviewed plan is wrong even if the observed Claude Code version never sends `ping`.

Result: held. `scratch/planner/ws/crates/cctg/src/channel.rs` routes unmatched requests, including `ping`, to `METHOD_NOT_FOUND = -32601`; its unit test explicitly expects that response, and `scratch/planner/ws/crates/cctg/tests/agent_stdio.rs` repeats the expectation. `OPEN_DECISIONS.md` instead requires an empty result for `ping`.

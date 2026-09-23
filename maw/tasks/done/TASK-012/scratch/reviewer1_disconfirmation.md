# Disconfirmation test

Concrete counter-example chosen before evaluating the plan: an npm-installed Claude Code process tree in which the hook's own Claude process and its enclosing Claude parent both appear as `node.exe`. If the reference implementation identifies the own process from `CLAUDE_PID` but recognizes a parent only by the executable name `claude`/`claude.exe`, a genuinely nested run will emit `parent_claude_pid: null` and be misclassified as top-level.

Evidence search to perform: inspect the reference `proctree` implementation and its tests, then compare it with the TASK-003 nesting contract and captures.

## Result

The counter-example held. In `scratch/planner/ws/crates/cctg/src/proctree.rs`, `lineage()` selects the nearest process named `claude` before consulting the exact `CLAUDE_PID`, and searches for a parent only by `is_claude(name)`. Its npm-oriented test uses `hook -> bash -> node(own, env pid) -> claude` and expects the farther native `claude` to become the own process; it has no `node(own) -> ... -> node(parent)` nested case. Therefore an npm-installed nested run cannot emit the real parent pid and may be classified as top-level. TASK-003 only verified native `claude.exe` and explicitly said other-platform/process names need separate verification.

# Reviewer 2 disconfirmation (written before evaluating PLAN_V2)

Counter-example chosen: PLAN_V2's own-pid fix ("an ancestor whose pid equals
CLAUDE_PID and which is closer than the first named claude wins") could
(a) change the answer for the native TASK-003 chains B/D/E, or
(b) let a stale CLAUDE_PID that happens to equal an intermediate bash/other
wrapper become the session's own pid, so SessionEnd carries a wrong pid.
Secondary: a mixed chain hook -> bash -> node.exe(own, CLAUDE_PID) -> bash ->
claude.exe(parent) still reports claude_pid = parent pid in the planner reference.

Search: planner reference proctree::lineage + tests, then new tests in
reviewer2/ws (see test names in PLAN_FINAL).

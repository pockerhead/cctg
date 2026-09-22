# Metrics — TASK-001

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2.5 | premise-challenge | codex | gpt-5.6-sol | medium | PREMISE SUSPECT | n/a | 197303 | 3825 | 201128 | 2m 41s |
| 2 | 3 | planner | claude | opus | medium | ok; log:malformed=2 (ts offset +03:00, not Z) | 44 | — | — | 160592 | 10m 45s |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | ok (websocket retries in stderr, recovered) | n/a | 1833615 | 18235 | 1851850 | 22m 39s |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | ok (one factual reversal corrected by orchestrator, see PLAN_FINAL 0.1) | 35 | — | — | 149520 | 10m 22s |
| **SUBTOTAL claude** | | | claude | | | | 79 | — | — | 310112 | 21m 07s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 2030918 | 22060 | 2052978 | 25m 20s |
| **TOTAL** | | 4 spawns / 4 agents | | | | | 79 | | | | 46m 27s |

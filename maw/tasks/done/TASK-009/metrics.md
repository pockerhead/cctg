# Metrics — TASK-009

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2.5 | premise-challenge | codex | gpt-5.6-sol | medium | PREMISE SUSPECT (no transcript path source; spec amended by orchestrator) | n/a | 1279627 | 5354 | 1284981 | 11m 33s |
| 2 | 3 | planner | claude | opus | medium | ok (reference, 132 tests, 12/12 mutations; 4 open questions, orchestrator decided) | 56 | — | — | 250662 | 19m 02s |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | ok (runner shell reaped for low memory; codex finished on its own) | n/a | 6861142 | 27461 | 6888603 | ~25m |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | ok (140 tests, 21/21 mutations; stale-offset loop fixed) | 54 | — | — | 210495 | 16m 19s |
| 5 | 6 | implementer | codex | gpt-5.6-sol | medium | IMPLEMENTED (140 tests; runner reaped for low memory, codex finished on its own) | n/a | 5660262 | 20793 | 5681055 | ~20m |
| 6 | 7 | code-reviewer | claude | opus | medium | PASS (6 minor) | 18 | — | — | 142958 | 4m 49s |
| 7 | 8 | fixer | codex | gpt-5.6-sol | medium | ok (5 fixes, 3 tests; candidate list no longer leaks project dirs) | n/a | 3635587 | 17833 | 3653420 | 14m 59s |
| 8 | 9 | qa | claude | opus | medium | SHIP | — | — | — | — | — |
| **SUBTOTAL claude** | | | claude | | | | 128 | — | — | 604115 | 40m 10s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 17436618 | 71441 | 17508059 | 26m 32s |
| **TOTAL** | | 8 spawns / 8 agents | | | | | 128 | | | | 66m 42s |

# Metrics — TASK-015

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2 | premise-challenge | codex | gpt-5.6-sol | medium | PREMISE HOLDS (65/65 Agent calls carry toolUseResult.agentId) | — | 2130601 | 11839 | 2142440 | ~3m |
| 2 | 3 | planner | claude | opus | medium | PLAN + reference ws, lib 296 passed, 8/8 mutations killed | — | — | — | — | — |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | PLAN_V2: unknown-send tombstone, legacy ghost entries, unbounded queues/index, body read races, header >4096, nested answer lifecycle | — | 2401413 | 15785 | 2417198 | 10m27s |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | PLAN_FINAL: 6 of 7 findings reproduced+fixed, 1 rejected, 2 own findings; lib 311, 18/18 mutations killed | 75 | — | — | 286358 | ~20m |
| 5 | 6 | implementer | claude | opus | medium | patch applied, hashes OK, lib 311 passed | — | — | — | — | — |
| 6 | 7 | code-reviewer | claude | opus | medium | NEEDS_WORK (1 major: indexes of ended sessions never freed; 3 minor) | — | — | — | — | — |
| 7 | 8 | fixer | claude | opus | medium | 1 major + 3 minor + nit + 2 tests; lib 315 passed | — | — | — | — | — |
| 8 | 9 | qa | claude | opus | medium | SHIP (own e2e via real hook ingress + TCP agents + restart; 2 mutations kill it) | 50 | — | — | 222348 | 8m49s |
| **SUBTOTAL claude** | | | claude | | | | 125 | — | — | 508706 | 0m 00s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 4532014 | 27624 | 4559638 | 0m 00s |
| **TOTAL** | | 8 spawns / 8 agents | | | | | 125 | | | | 0m 00s |

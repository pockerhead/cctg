# Metrics — TASK-017

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2 | premise-challenge | codex | gpt-5.6-sol | medium | interrupted (STATUS_DLL_INIT_FAILED); counter-example accepted by orchestrator, spec amended | — | 3010434 | 9624 | 3020058 | — |
| 2 | 3 | planner | claude | opus | medium | PLAN + reference (7 files), lib 388, buffer_e2e via serve_agents, 12/12 mutations | — | — | — | — | — |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | PLAN_V2: Resume only after first text, callback not bound to message/period, keyboard removal not retried, dead period vs buffer period, flush_all cost, live-path writes, weak tests | — | 2726931 | 20239 | 2747170 | 8m30s |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | PLAN_FINAL: stale agent after resume fixed (SessionEnd clears agent), 2 tests; findings 1-6 rejected with reasons; lib 390, 14/14 mutations | — | — | — | — | — |
| 5 | 6 | implementer | claude | opus | medium | patch applied, 7 hashes OK, lib 390 | — | — | — | — | — |
| 6 | 7 | code-reviewer | claude | opus | medium | PASS (2 minor Resume races; flake named: 2 pre-existing timing tests) | — | — | — | — | — |
| 7 | 8 | fixer | claude | opus | medium | period-bound Resume, press after later end, 2 flaky tests deterministic, 2 tests; lib 393 | — | — | — | — | — |
| 8 | 9 | qa | claude | opus | medium | SHIP (own e2e via serve_agents + route_batch, 7/7; 4/4 mutations; hub:: 21/21 under load) | 40 | — | — | 185461 | 21m36s |
| **SUBTOTAL claude** | | | claude | | | | 40 | — | — | 185461 | 0m 00s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 5737365 | 29863 | 5767228 | 0m 00s |
| **TOTAL** | | 8 spawns / 8 agents | | | | | 40 | | | | 0m 00s |

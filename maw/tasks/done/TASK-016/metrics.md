# Metrics — TASK-016

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2 | premise-challenge | codex | gpt-5.6-sol | medium | PREMISE SUSPECT (✍️ on uncorrelated UserPromptSubmit); spec amended | — | 1968995 | 6597 | 1975592 | — |
| 2 | 3 | planner | claude | opus | medium | PLAN + reference (23 files), lag measured, lib 341, 19/19 mutations | — | — | — | — | — |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | PLAN_V2: out-of-order results, offset advances on failed send, truncate loop, Stop barrier, tool errors as ok, foreign channel source, BOM, wire/reader bounds, path gate | — | 6235842 | 30550 | 6266392 | 20m13s |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | PLAN_FINAL: 10 findings fixed, 1 rejected (is_error on 9661 real results), 2 own; lib 358, 35/35 mutations | — | — | — | — | — |
| 5 | 6 | implementer | claude | opus | medium | patch applied, 23 hashes OK, lib 358 passed | — | — | — | — | — |
| 6 | 7 | code-reviewer | claude | opus | medium | NEEDS_WORK (M1 barrier queue grows while Telegram waits; 3 minor) | — | — | — | — | — |
| 7 | 8 | fixer | claude | opus | medium | M1 + m2 + interrupt note + 3 tests; lib 361 passed | — | — | — | — | —|
| 8 | 9 | qa | claude | opus | medium | NO_SHIP (B1 ingress drops TranscriptChunk; M1 resume mid-line resends history; M2 refused line reordered) | — | — | — | — | — |
| 9 | 8 | fixer (round 2) | claude | opus | medium | B1 + M1 + M2 + ends_unclaimed fixed; stream_e2e (6) via real TCP + agent binary; lib 368 | — | — | — | — | — |
| 10 | 9 | qa (round 2) | claude | opus | medium | SHIP (own 9-test e2e via real agent + TCP; BUG-1 minor -> TASK-023) | 80 | — | — | 291765 | 36m |
| **SUBTOTAL claude** | | | claude | | | | 80 | — | — | 291765 | 0m 00s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 8204837 | 37147 | 8241984 | 0m 00s |
| **TOTAL** | | 10 spawns / 10 agents | | | | | 80 | | | | 0m 00s |

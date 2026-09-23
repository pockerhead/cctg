# Metrics — TASK-011

| # | Step | Agent | Provider | Model | Effort | Outcome | Tool uses | In-tok | Out-tok | Total-tok | Duration |
|---|------|-------|----------|-------|--------|---------|-----------|--------|---------|-----------|----------|
| 1 | 2.5 | premise-challenge | codex | gpt-5.6-sol | medium | PREMISE SUSPECT (Windows folder spellings; spec amended by orchestrator) | n/a | 1037640 | 7135 | 1044775 | 9m 47s |
| 2 | 3 | planner | claude | opus | medium | ok (reference, 221 tests, 11/12 mutations + 1 equivalent) | 76 | — | — | 351844 | 22m 31s |
| 3 | 4 | plan-reviewer-1 | codex | gpt-5.6-sol | medium | ok (6 defects; runner reaped for low memory, codex finished on its own) | n/a | 7206656 | 32932 | 7239588 | ~30m |
| 4 | 5 | plan-reviewer-2 | claude | opus | medium | ok (7/7 defects reproduced and fixed; 231 tests; 22/23 mutations + 1 equivalent) | 50 | — | — | 253040 | 18m 17s |
| 5 | 6 | implementer | codex | gpt-5.6-sol | medium | IMPLEMENTED (231 tests) | n/a | 2743194 | 9761 | 2752955 | 11m 3s |
| 6 | 7 | code-reviewer | claude | opus | medium | NEEDS_WORK (/clear jumps slot in real hook order; nested resume rewrites a top-level kind) | 29 | — | — | 183964 | 7m 31s |
| 7 | 8 | fixer | codex | gpt-5.6-sol | medium | incomplete (0xC0000142 under memory pressure; partial registry.rs + regression tests left in tree) | n/a | 5966616 | 19281 | 5985897 | ~25m |
| 8 | 8 | fixer (continuation) | claude | opus | medium | ok (6/6 decisions, 239 tests) | 49 | — | — | 168358 | 6m 23s |
| 9 | 9 | qa | claude | opus | medium | NEEDS_FIXES (B1 nested SessionEnd kills live top-level; B2 pid rule not limited to source=clear) | 43 | — | — | 226972 | 12m 44s |
| 10 | 8 | fixer round 2 | claude | opus | medium | ok (B1, B2, O3; 244 tests; QA harness 23/23) | 46 | — | — | 144970 | 7m 44s |
| **SUBTOTAL claude** | | | claude | | | | 293 | — | — | 1329148 | 75m 10s |
| **SUBTOTAL codex** | | | codex | | | | n/a | 16954106 | 69109 | 17023215 | 20m 50s |
| **TOTAL** | | 10 spawns / 10 agents | | | | | 293 | | | | 96m 00s |

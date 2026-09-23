# Open decisions — TASK-015 (orchestrator, full autonomy)

- 2026-09-24: planner question 1 (subagents of nested runs): not shown. The nested block is the unit the user sees; its inner agents would be noise.
- 2026-09-24: planner question 2 (target_agent on a reply to a finished subagent's block): kept. SendMessage to a finished agent resumes it in Claude Code, so the reply is meaningful.
- 2026-09-24: planner question 3 (nested block body): shows the nested run's last answer.
- 2026-09-24: planner question 4 (correlation window): 60 s, `Options.correlate_for`.
- 2026-09-24: remote-device sessions get no subagent blocks until step 5 (the hub reads transcripts locally); accepted, recorded as a known limitation.
- 2026-09-24 (after plan-reviewer-1): block first send is at-most-once. A send whose outcome is unknown at restart is not repeated; it becomes a registry tombstone, no Telegram mark (Bot API has no idempotency key). This is the reading of acceptance criterion 6 ("no duplicates; unfinished blocks marked deterministically" for blocks with a known message id).
- 2026-09-24: reviewer-1 findings 1-7 go to plan-reviewer-2 for verification; each is fixed only if reproduced by a failing test or a concrete code path, with the smallest bound that closes it (reuse existing caps and the dispatch pattern, no new frameworks).
- 2026-09-24: QA SHIP. Minors left as documented behaviour: a second SubagentStop without a new handback replaces a shown report with last_assistant_message (only on resume); a >4096 body document may arrive before its block; an empty meta description does not fall back to the Agent call description.

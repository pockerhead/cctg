# PCTX proposals — TASK-015 (planner)

## 2026-09-24 — transcript domain: SubagentInput.description and Subagent::header

The transcript domain lists `SubagentInput { agent_id, agent_type, meta, report, transcript, last_assistant_message }`. TASK-015 adds an optional `description` (the parent `Agent` call's, used when the meta has none) and a public `Subagent::header()` (first line of `render`). Proposed update of that invariant line, so later tasks do not treat the struct as fixed.

## 2026-09-24 — hub domain: subagent and nested blocks (TASK-015 summary line)

Proposed line: "TASK-015 (`hub/subagents.rs` + slots + registry): a typed subagent hook is only a candidate until the parent transcript shows an `Agent` call whose result carries its `toolUseResult.agentId` (incremental per-session scan, lookups after 1, 2, 4, 8, 16 s within 60 s, a stop re-opens the window); only then `registry.subagents` gets the entry and one block `↳ <type> <id>: <description>` + `в работе…`, edited on `SubagentStop` to `Subagent::render` (cut to 4096 + a document when longer). A nested run with a known parent gets one `⇣ nested <id>` block, finished with its last Stop answer at its SessionEnd. Block text is persisted until Telegram took it; a first send without an answer is never repeated after a restart; running blocks of ended sessions become `итог не получен`. A reply to a subagent block of the slot's live session adds meta `target_agent`. Subagents of nested runs get no block. Blocks need the parent transcript on the hub's machine (remote devices: none until the agent serves files)."

## 2026-09-24 — risk lesson: blocks are local-only for now

Subagent correlation reads the parent transcript from `transcript_path`, which the hub can read only for sessions on its own machine. Until an agent can serve transcripts, sessions on a second device (dev order step 5) get no subagent blocks. Worth a line in the hub domain so step 5 plans for it.

## 2026-09-24 — plan-reviewer-2: amend the proposed hub TASK-015 line

On top of the planner's proposed line: a block's first send is at most once in-process too, not only across a restart: only a 4xx refusal or a connection that never opened is retried; any other outcome (5xx, unreadable answer, message id 0, timeout, no answer) leaves a registry tombstone (`sending`, no text) that is never sent. Bounds: 16 block jobs in flight (the rest wait in `registry.json`), 2 subagent file reads at a time with the newest stop per agent winning, 1024 calls/results per session Agent index, 1024 durable subagent records (oldest settled evicted; a reply to an evicted block has no `target_agent`). A nested run's last answer is persisted in its block until the run ends. `registry.json` subagent records without a block header (TASK-011 era) are dropped at load.

> RESOLVED: folded into domains/transcript.md and domains/hub.md (implemented line with reviewer-2 amendments, risk lesson) on 2026-09-24.

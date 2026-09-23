## Counter-example tested

An explicit `Agent` call can finish without any parent-transcript result that contains `toolUseResult.agentId`; if the only reliable `agent_id` then exists in the typed `SubagentStop`, requiring a parent-result correlation would discard a real explicit subagent rather than merely suppress unmatched internal noise.

## Primary-source investigation

I opened the raw anonymized parent-transcript fixture at `crates/transcript/tests/fixtures/final_answer.jsonl:10`. Its `tool_result` record contains both the displayed agent id and the record-level object `toolUseResult.status = "async_launched"` with the same non-empty `toolUseResult.agentId`.

I then ran this corpus check against every top-level JSONL in the live cctg project transcript directory (the script reads JSON records, keys every assistant `Agent` tool call by tool-use id, and matches its user `tool_result` record):

```text
powershell.exe -NoProfile -File maw/tasks/in_progress/TASK-015/scratch/audit_agent_links.ps1
```

Real output:

```text
parent_jsonl_files=13
invalid_json_lines=0
agent_calls=65
calls_with_result=65
calls_with_agent_id=65
calls_without_result=0
results_without_agent_id=0
```

## Did it hold

No. The raw fixture exhibits the proposed correlation, and the executable scan found no completed or outstanding explicit `Agent` call lacking either its matching result or a non-empty `toolUseResult.agentId`: all 65 observed calls had both. Thus the concrete failure case needed to show that correlation would suppress a real explicit subagent was not present in the primary sources checked.

## Verdict

PREMISE HOLDS — live-transcript corpus scan command above: `agent_calls=65`, `calls_with_result=65`, `calls_with_agent_id=65`, `calls_without_result=0`, `results_without_agent_id=0`; corroborated by raw fixture `crates/transcript/tests/fixtures/final_answer.jsonl:10`

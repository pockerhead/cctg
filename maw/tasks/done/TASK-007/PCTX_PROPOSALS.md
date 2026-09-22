# PCTX proposals — TASK-007

## 2026-09-23 (planner): subagent artifact facts for domains/transcript.md

Surveyed all 538 subagent files and 76 parent transcripts under `~/.claude/projects` (scripts and outputs in `scratch/planner/survey_*.{py,out.txt}`):

- `.meta.json` always has string `agentType`, `description`, `toolUseId` and int `spawnDepth`; `model`, `requestShape`, `requestNonInteractive`, `parentAgentId`, `stoppedByUser` are optional. `toolUseId` equals the parent `Agent` tool_use id, `agentType` equals its `subagent_type`, `description` equals its `description` (419/419 linked calls).
- No parent transcript contains `isSidechain: true` user/assistant records (0/76); every `Agent` result is `status: async_launched` with `toolUseResult.agentId`.
- The first record of every subagent jsonl is the spawn prompt as a non-meta string (538/538), median 3273 chars, 212 over 4096.
- A subagent that calls `SubagentHandback` also records it in its own jsonl as a `tool_use` with `input.message` (222/538 files); the text after it is a short farewell (median 200 chars). This is a second copy of the report, reachable without the hook.

Why: TASK-015 and later hub tasks will rediscover these otherwise.

> RESOLVED: folded into domains/transcript.md on 2026-09-23.

# PCTX proposals — TASK-005

- 2026-09-22 (planner): domain `transcript` record-type list lacks `cost-state` (13 records in this project's transcripts, `scratch/survey_shapes.out.txt`). Harmless under the allowlist, but worth listing so nobody treats it as a new format.
- 2026-09-22 (planner): add to domain `transcript`: every real `assistant` record carries exactly one content block; one API response is split over several records sharing `message.id` (432 of 602 ids span >1 record), and thinking sits in its own record. `user` `message.content` is a string in ~12% of records (prompts and `isMeta` records). Renderers must not assume one record = one response.
- 2026-09-22 (planner): add to domain `transcript`: the id of a subagent spawned by an `Agent` call is in the result record's top-level `toolUseResult.agentId`; `toolUseResult` is a string (not an object) on error results.
- 2026-09-22 (qa): add to domain `transcript`: inbound channel messages (`<channel source=...>` text) land in the jsonl as `user` records with `isMeta: true` (3 of 3 real samples, probe sessions only). A renderer that hides every `isMeta` turn would hide Telegram-originated prompts; TASK-006 should distinguish them, not filter `isMeta` wholesale.

> RESOLVED: all four folded into domains/transcript.md on 2026-09-22.

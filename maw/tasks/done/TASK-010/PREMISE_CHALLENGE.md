## Counter-example tested

The hook payloads may contain no stable event identifier, so the acceptance predicate for repeat delivery being idempotent by event key could be satisfied only by inventing a key that conflates distinct legitimate hook events; if the actual hook contract has no stable per-event identity, the task premise is incomplete.

## Primary-source investigation

Pending.

## Did it hold

Pending.

## Verdict

Pending.

## Orchestrator completion (2026-09-23)

The codex run stopped before filling the sections: under host memory pressure 13 process spawns failed with 0xC0000142 and the file tool could not write. The orchestrator checked the recorded counter-example against primary evidence, the redacted real hook payloads in `maw/tasks/done/TASK-003/scratch/capture_*.jsonl`: `SessionStart` keys = cwd, hook_event_name, model, scratchpad_dir, session_id, source, transcript_path; `Stop` keys = background_tasks, cwd, hook_event_name, last_assistant_message, permission_mode, prompt_id, session_crons, session_id, stop_hook_active, transcript_path. Neither has a per-event id, and a resumed session legitimately repeats `SessionStart` with the same session_id.

Verdict: PREMISE SUSPECT — the idempotency criterion has no natural key in the real payloads; smallest reframing: a hook-minted `event_id` per invocation, deduplicated by the hub in a bounded window. Resolved by amending task.md / TASK_FINAL.md.

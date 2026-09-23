# -*- coding: utf-8 -*-
# Appends reviewer-2 decision / dead_end entries to the task log (append only).
import datetime, io, json, os
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='plan-reviewer-2', provider='claude', model='opus', effort='medium')
S, R, A = 'crates/cctg/src/hub/slots.rs', 'crates/cctg/src/hub/registry.rs', 'crates/cctg/src/hub/subagents.rs'
entries = [
 ('dead_end', "Reviewer-1 finding 5 (running header over 4096) rejected: Subagent::new passes type and description through transcript one_line, which cuts to SUMMARY_CHARS=120, and is_agent_id caps the id at 64, so the header stays far below 4096; regression test a_huge_call_description_still_fits_one_message passes on the planner code. Only the in-memory copy of the call description in AgentIndex is now cut (256 UTF-16 units).", [A, 'crates/transcript/src/render.rs']),
 ('decision', "At-most-once first send also in-process: a block Send is retried only when Telegram refused it with a 4xx or the connection never opened (reqwest is_connect); any other outcome (5xx, decode error, message_id 0, timeout, no answer) becomes the registry tombstone (sending stays true, no pending, not running). Alternative: the planner's retry of every failed Send up to 5 times, which sent a duplicate block in test a_first_send_with_an_unclear_answer_is_not_sent_again.", [S, R]),
 ('decision', "Bounds reuse the existing registry-pending pattern: MAX_BLOCK_JOBS=16 block jobs in flight (the rest stay pending in registry.json), MAX_BODY_READS=2 subagent file reads with the newest stop per agent winning, MAX_INDEX_ENTRIES=1024 calls and links per session index (FIFO), MAX_SUBAGENTS=1024 durable subagent records (evict the oldest settled one by a new seen field, refuse when all are running or pending). Alternative: a generation counter per body key plus separate bounded channels (more state for the same guarantee).", [S, R, A]),
 ('decision', "Nested run's last answer is persisted in Block.answer (serde default, skipped when None), taken at the run's SessionEnd and dropped when the block is lost; replaces the memory-only nested_answers map. Alternative: edit the block with the answer at each nested Stop (extra metered edits per turn and the end could not tell a shown answer from none).", [R, S]),
 ('decision', "Legacy registry.json subagent records (TASK-011 wrote one for every typed hook, no block) are dropped at load when block.header is empty; they are not trusted as correlated. Alternative: bump registry VERSION (would stop the hub on existing files) or keep them (a later typed stop of the same id posted a ghost block, reproduced by a_legacy_subagent_record_never_becomes_a_block).", [R]),
 ('decision', "A subagent matched only after its parent session ended, with no stop seen, is marked 'итог не получен' at once instead of staying 'в работе…' forever. Alternative: accept candidates only from live sessions (would drop real subagents whose stop comes after the parent's end).", [S]),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for kind, body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, kind=kind, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries))

import datetime, io, json, os
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='planner', provider='claude', model='opus', effort='medium')
S, P = 'crates/cctg/src/hub/slots.rs', 'crates/cctg/src/hub/permissions.rs'
entries = [
 ('decision', "A button press finds its prompt by the Telegram message_id of the callback (hub map message_id -> prompt key); callback_data stays 'allow:<id>'/'deny:<id>' (<=11 bytes) and its request id must match the prompt. Alternative: a hub-minted token in callback_data (fits 64 bytes too, but the task fixes the data to action + request id and the message id already disambiguates sessions).", [P, S]),
 ('decision', "Prompts are routed by the requesting session's own slot (registry.sessions[session].slot -> topic), not by the slot's current live session, and are held until that slot has a topic. Alternative: reuse the reply rule live_reply_slot (drops prompts of a session the slot no longer shows while the claude process is blocked on them).", [S]),
 ('decision', "Prompt sends bypass MAX_QUEUED_MESSAGES (own Work::Permission, bounded by MAX_PROMPTS=256) and go out as Op::Send{permission:true}; no second dispatch task. Alternative: a separate permission dispatch channel (only matters when the scheduler mpsc of 1024 is full, which happens only while a Telegram call hangs; the scheduler drains the whole mpsc on each loop).", [S, 'crates/cctg/src/hub/scheduler.rs']),
 ('decision', "Decision edit sends an explicit empty inline keyboard. Bot API docs page (via fetch) says editMessageText without reply_markup removes the keyboard, a search summary said it is kept; the explicit value removes it either way. Alternative: omit reply_markup (depends on which reading is right).", [P]),
 ('decision', "Prompts are not closed on Stop/UserPromptSubmit: a background subagent can hold a permission prompt after the main turn's Stop, so a Stop does not prove the prompt was answered. Only the waiting icon follows those hooks (existing registry behaviour). Alternative: expire open prompts on Stop (would remove live buttons).", [S, 'crates/cctg/src/hub/registry.rs']),
 ('decision', "Verdict target: the connection that relayed the prompt, else the newest live connection of the same host+claude_pid (agent reconnected after a link drop); if none or try_send fails, answer 'session offline' and keep the prompt undecided. Alternative: drop the verdict when the original conn is gone (a link blip would make the buttons useless).", [S]),
 ('dead_end', "The overtake test first recognised the prompt by the permission flag, so mutation M2 (permission:false) was killed only by a helper, not by ordering. The test now finds the prompt by its text and settles on 'prompt sent or 3 sends'.", [S, 'maw/tasks/in_progress/TASK-014/scratch/planner/mutations.out.txt']),
 ('dead_end', "First workspace run failed hook_cli settings_snippet test: ws/ copy lacked docs/hook-settings.json. docs/ added to ws; not a code issue.", ['maw/tasks/in_progress/TASK-014/scratch/planner/workspace_test.txt']),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for kind, body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, kind=kind, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries))

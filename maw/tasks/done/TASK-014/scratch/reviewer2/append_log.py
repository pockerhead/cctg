import datetime, io, json, os
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='plan-reviewer-2', provider='claude', model='opus', effort='medium')
W, S, P, A = 'crates/cctg/src/wire.rs', 'crates/cctg/src/hub/slots.rs', 'crates/cctg/src/hub/permissions.rs', 'crates/cctg/src/agent.rs'
entries = [
 ('decision', "Verdict delivery ack stays on wire v1: optional Register.verdict_ack, optional HubMsg::PermissionVerdict.verdict_id (skipped when None, byte-identical legacy line), new AgentMsg::PermissionAck sent only in reply to a verdict with an id. Old agents keep hand-off-as-delivery. Alternative: PLAN_V2 VERSION=2 with a mandatory id (check_version would reject every agent still running in a live session after a hub upgrade).", [W, A, S]),
 ('decision', "No per-connection in-flight tracking: a Selected prompt is pushed again on reconnect of an agent of its session, on each press and on the retry tick; the agent keeps a 256-id cache across reconnects, passes each id once and acks every copy. Alternative: PLAN_V2 in_flight_conn bookkeeping cleared on Disconnected (more state, same guarantee).", [S, A]),
 ('decision', "Prompts are closed by a sweep after every hook over sessions the registry marks ended (own SessionEnd, /clear takeover, reused pid), never on an ignored nested-resume SessionEnd. Alternative: react only to an accepted SessionEnd variant (misses /clear when the new SessionStart arrives first and pid reuse).", [S]),
 ('decision', "Full prompt book: forget the oldest finished prompt, else expire the oldest open (not selected, not send-in-flight) prompt with one edit to 'Запрос устарел' and no buttons, else refuse. Alternative: PLAN_V2 refuse when all 256 are active (terminal-answered prompts stay open until SessionEnd, so one long session would turn the relay off for all sessions).", [P, S]),
 ('decision', "Waiting icon = any active prompt of the session raised after its last Stop/UserPromptSubmit (Prompt.waits, cleared by quiet()). Alternative: all active prompts (a terminal-answered prompt, never reported, would hold the icon after the next Telegram decision until the turn ended).", [P, S]),
 ('dead_end', "First pid-fallback repro (agent of unknown session C on the live pid of A) also 'failed' on the fixed code: agent_session() binds such an agent to A by the pid map, so it is the right target. The repro now starts A without claude_pid, leaving pid 10 unmapped.", [S, 'maw/tasks/in_progress/TASK-014/scratch/reviewer2/phase1_failing.txt']),
 ('dead_end', "Edit::Failed(n) lost its counter when the retry set the state to Due/InFlight, so the give-up limit never triggered; caught by the unit test. Counter moved to Prompt.edit_failures.", [P]),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for kind, body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, kind=kind, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries))

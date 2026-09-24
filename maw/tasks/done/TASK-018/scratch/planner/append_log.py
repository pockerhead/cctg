import datetime, io, json, os
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='planner', provider='claude', model='opus', effort='medium')
H, SP, AG, SK = 'crates/cctg/src/hook.rs', 'crates/cctg/src/spool.rs', 'crates/cctg/src/agent.rs', 'crates/cctg/tests/soak.rs'
entries = [
 ('decision', "Spool keeps only SessionStart and SessionEnd (ids, pids, folder, transcript path; no prompt or answer text). Turn answers (Stop), prompts and subagent events of a period without hub stay lost as today. Alternative: spool every event with free text stripped, or keep Stop answers (late answers out of order, text on disk).", [SP]),
 ('decision', "One file per event under <state>/spool/<session>/<nanos>-<event_id>.json written as .tmp and renamed (maildir); the file keeps the HookPost with its event_id so the hub's dedup drops a copy. Alternative: one appended jsonl per session (needs a cross-process lock; a partial line or a concurrent delete loses events).", [SP]),
 ('decision', "Replay is per session: every hook of the session replays its spool first (stops at the first failure, own event then kept or dropped, never sent ahead), and the agent replays its session's spool after every registration. Alternative: hook-only replay (an idle session stays invisible until the next prompt) or a device-wide replay of all sessions (cross-session races, stale starts of dead sessions could create topics).", [H, AG]),
 ('decision', "Replay and the own POST share one deadline = the event's existing POST timeout, so a down hub costs a hook no more than before (SessionEnd budget unchanged). Alternative: a separate replay budget (up to 2x the timeout inside SessionEnd's shared 1.5 s).", [H]),
 ('decision', "Device state dir = absolute CCTG_STATE_DIR (process env or device.env), else <home>/.cctg; a relative value is ignored because hooks run in the session folder. Alternative: honour relative values like the hub does (spool would land inside project folders).", ['crates/cctg/src/device.rs']),
 ('decision', "Soak simulates Claude Code with stand-in processes named claude.exe (a copy of the soak test binary) started through a launcher that exits, so the real cctg hook walks a real process tree: top-level stand-ins end the chain, the nested stand-in is a child of A1. Hooks and agents are children of their stand-in (agent Register.claude_pid = hook claude_pid). Alternative: inject HookPosts directly (skips the hook binary and the nesting walk) or spawn hooks from the test (under Claude Code every hook would see the orchestrator's claude as its own).", [SK]),
 ('decision', "Soak is a harness=false test target that runs only with -- --ignored and otherwise prints 'skipped'; the same binary re-executes itself as launcher/stand-in by env role. Alternative: libtest #[ignore] plus re-exec through a test filter (libtest writes to stdout, which the stand-in protocol uses).", [SK, 'crates/cctg/Cargo.toml']),
 ('dead_end', "A burst of tool-call lines only does not build a queue the permission prompt can overtake: the scheduler merges consecutive tool lines of a topic into one message when more wait than tokens, so 60 tool lines drained in a few sends. The soak burst therefore uses typed prompts (never merged) plus tool lines.", [SK, 'crates/cctg/src/hub/scheduler.rs']),
 ('dead_end', "Counting the A #2 stream lines sent after its permission prompt right when the prompt went out gave 0 (they had not been sent yet); the count is taken after the burst drained.", [SK]),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for kind, body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, kind=kind, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries), ts)

import datetime, io, json, os

LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='planner', provider='claude', model='opus', effort='medium', kind='decision')
entries = [
    ("Offset is saved before the routed batch is handed to handlers (at-most-once): a crash between save and handling drops those commands instead of answering them twice. Alternative: save after handling (at-least-once, duplicate replies after a crash).",
     ["crates/cctg/src/hub/updates.rs", "crates/cctg/src/hub/offset.rs"]),
    ("`[n]` means the last n prompts; slicing is a new pure `transcript::last_prompts` that reuses the renderer's prompt predicate. Alternative: cut the rendered text by `> ` lines in hub (breaks on service lines in full, duplicates prompt rules).",
     ["crates/transcript/src/render.rs"]),
    ("Resolver seam is `TranscriptLocator::locate(thread_id, session_prefix)`; `ProjectsDir` ignores thread_id today, TASK-011 keys it by topic. Alternative: a free function over the projects root with prefix only (TASK-011 would have to change every call site).",
     ["crates/cctg/src/hub/sessions.rs"]),
    ("Commands run on one sequential worker fed by an unbounded mpsc from the poll callback; file IO and parsing go to spawn_blocking. Alternative: tokio::spawn per command (chunks of two replies interleave in one topic).",
     ["crates/cctg/src/hub/commands.rs", "crates/cctg/src/hub/mod.rs"]),
    ("Projects root is `CCTG_PROJECTS_DIR` or `<USERPROFILE|HOME>/.claude/projects`; CLAUDE_CONFIG_DIR is not read. Alternative: derive from CLAUDE_CONFIG_DIR (may be a comma list, the hub is not a Claude child process).",
     ["crates/cctg/src/hub/config.rs"]),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries))

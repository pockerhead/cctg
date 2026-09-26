"""Appends the implementer's decision entries to log.jsonl (BOM-free UTF-8)."""
import datetime
import json

LOG = 'maw/tasks/in_progress/TASK-053/log.jsonl'
now = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = {'stage': 'implementer', 'provider': 'anthropic', 'model': 'opus', 'effort': 'medium'}
entries = [
    {
        'kind': 'decision',
        'body': 'Compaction end = SessionStart(source=compact) as the spec says, not the documented PostCompact hook (it exists: trigger + compact_summary). Alternative PostCompact would need one more hook group and carries the summary text we must not ship.',
        'refs': ['crates/cctg/src/hub/slots.rs:compact_ended', 'maw/tasks/in_progress/TASK-053/scratch/hooks_doc.md:3056'],
    },
    {
        'kind': 'decision',
        'body': 'Done line waits up to COMPACT_NUMBERS_WAIT=10s for the first StatusLine whose context differs from the last one before the end (before refreshed at SessionStart compact), else goes without percentages. Alternative: send at once with before only (no after known then).',
        'refs': ['crates/cctg/src/hub/slots.rs:compact_numbers', 'crates/cctg/src/hub/slots.rs:check_compactions'],
    },
    {
        'kind': 'decision',
        'body': 'Status timer in whole minutes (next_deadline wakes at each minute), so a compaction costs about one edit per minute. Alternative: seconds timer, an edit every status_every (5s).',
        'refs': ['crates/cctg/src/hub/status.rs:Phase::Compacting', 'crates/cctg/src/hub/slots.rs:compaction_deadlines'],
    },
    {
        'kind': 'decision',
        'body': 'PreCompact hook group gets timeout 5 s and the short 300/600 ms POST budget; hook never prints or exits non-zero, so it can never block compaction. Alternative: default timeout (600 s) like the other lifecycle hooks.',
        'refs': ['docs/hook-settings.json', 'install.sh', 'crates/cctg/src/hook.rs:post_timeout'],
    },
]
with open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for entry in entries:
        f.write(json.dumps({'ts': now, **base, **entry}, ensure_ascii=False) + '\n')
print(now)

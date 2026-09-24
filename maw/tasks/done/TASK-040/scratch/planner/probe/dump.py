"""TASK-040 probe (TEMPORARY, copied from TASK-029): statusLine command and hook dumper.
Usage: python dump.py <kind>. Appends one JSON line to <dir>/events.jsonl with
the time, the kind, the key structure of stdin and selected short fields
(no prompt or message text). For kind=statusline also prints one short line
and records shell hints from env."""
import json, os, sys, time

D = os.path.dirname(os.path.abspath(__file__))
kind = sys.argv[1] if len(sys.argv) > 1 else '?'
raw = sys.stdin.read()
try:
    d = json.loads(raw)
except Exception:
    d = {}


def shape(v, depth=0):
    if isinstance(v, dict) and depth < 3:
        return {k: shape(x, depth + 1) for k, x in v.items()}
    if isinstance(v, list):
        return 'list[%d]' % len(v)
    if isinstance(v, (int, float, bool)) or v is None:
        return v
    return 'str[%d]' % len(v)


rec = {'t': time.time(), 'kind': kind, 'pid': os.getpid()}
if kind == 'statusline':
    rec['shape'] = shape(d)
    rec['session_id'] = d.get('session_id')
    rec['model'] = d.get('model')
    rec['effort'] = d.get('effort')
    rec['ctx'] = (d.get('context_window') or {}).get('used_percentage')
    rec['rate_limits'] = d.get('rate_limits')
    rec['env'] = {k: os.environ.get(k) for k in ('SHELL', 'MSYSTEM', 'EXEPATH', 'CLAUDE_CODE_GIT_BASH_PATH',
                                                  'CLAUDE_PID', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CONFIG_DIR')}
    rec['env_path_head'] = (os.environ.get('PATH') or '').split(os.pathsep)[:4]
else:
    rec['hook'] = d.get('hook_event_name')
    rec['session_id'] = d.get('session_id')
    rec['keys'] = sorted(d.keys())
    rec['tool_name'] = d.get('tool_name')
    rec['tool_use_id'] = d.get('tool_use_id')
    ti = d.get('tool_input') or {}
    if isinstance(ti, dict):
        rec['tool_input_keys'] = sorted(ti.keys())
        rec['description'] = ti.get('description') if isinstance(ti.get('description'), str) else None
        rec['subagent_type'] = ti.get('subagent_type')
        rec['run_in_background'] = ti.get('run_in_background')
    tr = d.get('tool_response')
    if isinstance(tr, dict):
        rec['tool_response_keys'] = sorted(tr.keys())
        for k in ('backgroundTaskId', 'interrupted', 'isBackgrounded', 'status', 'agentId', 'backgroundedByUser'):
            if k in tr:
                rec['tr_' + k] = tr[k] if not isinstance(tr[k], str) or len(tr[k]) < 80 else 'str'
    for k in ('agent_id', 'agent_type', 'stop_hook_active', 'is_interrupt', 'reason', 'error', 'source'):
        if k in d:
            v = d[k]
            rec[k] = v if not isinstance(v, str) or len(v) < 120 else 'str[%d]' % len(v)
with open(os.path.join(D, 'events.jsonl'), 'a', encoding='utf-8') as f:
    f.write(json.dumps(rec) + '\n')
if kind == 'statusline':
    sys.stdout.write('probe-statusline ctx:%s' % rec['ctx'])

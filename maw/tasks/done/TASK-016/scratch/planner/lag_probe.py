# Measures how long after its `timestamp` each transcript record becomes
# visible in the jsonl of a `claude -p` run. One fixed probe folder, env
# stripped of the inherited session vars (else no transcript is written),
# user settings not loaded (--setting-sources project), no hooks, no hub.
import datetime, json, os, subprocess, sys, time, uuid
here = os.path.dirname(os.path.abspath(__file__))
probe = os.path.join(os.environ['TEMP'], 'cctg-t016-probe')
os.makedirs(probe, exist_ok=True)
run = sys.argv[1] if len(sys.argv) > 1 else '1'
sid = str(uuid.uuid4())
enc = ''.join('-' if c in ':\/ _' else c for c in os.path.realpath(probe))
path = os.path.join(os.path.expanduser('~'), '.claude', 'projects', enc, sid + '.jsonl')
env = {k: v for k, v in os.environ.items()
       if k not in ('CLAUDE_CODE_CHILD_SESSION', 'CLAUDECODE', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_PID',
                    'CLAUDE_CODE_ENTRYPOINT', 'CLAUDE_CODE_SESSION_ATTENDED')}
prompt = ('Make exactly these three Bash tool calls, one per call, in order, never in parallel: '
          '`sleep 3 && echo one`, then `sleep 3 && echo two`, then `echo three`. '
          'Before each call write one short sentence. Then answer with the single word done.')
cmd = ['claude', '-p', prompt, '--session-id', sid, '--setting-sources', 'project',
       '--allowedTools', 'Bash', '--output-format', 'text']
si = subprocess.STARTUPINFO(); si.dwFlags |= subprocess.STARTF_USESHOWWINDOW; si.wShowWindow = 0
t0 = time.time()
p = subprocess.Popen(' '.join('"%s"' % c if ' ' in c or '`' in c else c for c in cmd) if False else cmd,
                     cwd=probe, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                     startupinfo=si, creationflags=subprocess.CREATE_NO_WINDOW, shell=False)
rows, buf, pos = [], b'', 0
def poll():
    global buf, pos
    if not os.path.exists(path):
        return
    with open(path, 'rb') as f:
        f.seek(pos); chunk = f.read()
    pos += len(chunk); buf += chunk
    seen = time.time()
    while b'\n' in buf:
        line, buf = buf.split(b'\n', 1)
        try:
            rec = json.loads(line)
        except Exception:
            rows.append(dict(seen=seen, type='?unparsable')); continue
        ts = rec.get('timestamp')
        t = datetime.datetime.fromisoformat(ts.replace('Z', '+00:00')).timestamp() if ts else None
        kinds = []
        c = (rec.get('message') or {}).get('content')
        if isinstance(c, list):
            kinds = [b.get('type') for b in c if isinstance(b, dict)]
        elif isinstance(c, str):
            kinds = ['string']
        rows.append(dict(seen=seen, ts=t, type=rec.get('type'), kinds=kinds,
                         stop=(rec.get('message') or {}).get('stop_reason')))
while p.poll() is None:
    poll(); time.sleep(0.1)
end = time.time()
for _ in range(30):
    poll(); time.sleep(0.1)
out, err = p.communicate()
lines = ['run %s rc=%s wall=%.1fs file_exists=%s records=%d' % (run, p.returncode, end - t0, os.path.exists(path), len(rows))]
for r in rows:
    lag = (r['seen'] - r['ts']) if r.get('ts') else None
    lines.append('%7.2fs  lag=%s  %-12s %-28s stop=%s' % (r['seen'] - t0, '%.2fs' % lag if lag is not None else '   -  ',
                 r['type'], ','.join(r.get('kinds') or []), r.get('stop')))
lines.append('exit_seen=%.2fs' % (end - t0))
lines.append('stdout: ' + out.decode('utf-8', 'replace').strip()[:200])
if err: lines.append('stderr: ' + err.decode('utf-8', 'replace').strip()[:300])
txt = '\n'.join(lines) + '\n'
open(os.path.join(here, 'lag_probe.run%s.out.txt' % run), 'w', encoding='utf-8', newline='\n').write(txt)
print(txt)

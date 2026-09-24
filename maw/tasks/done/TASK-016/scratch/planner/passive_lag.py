# Passive lag probe on a LIVE interactive Claude Code process: tails the
# given jsonl files (only bytes appended after start) every 100 ms and logs,
# per new record, the delay between its `timestamp` and the moment it became
# visible. Prints record types only, never content or paths.
import datetime, json, os, sys, time
here = os.path.dirname(os.path.abspath(__file__))
secs = float(sys.argv[1]); label = sys.argv[2]; files = sys.argv[3:]
pos = {f: os.path.getsize(f) for f in files}; buf = {f: b'' for f in files}
rows = []; t0 = time.time()
while time.time() - t0 < secs:
    for i, f in enumerate(files):
        size = os.path.getsize(f)
        if size <= pos[f]:
            continue
        with open(f, 'rb') as h:
            h.seek(pos[f]); chunk = h.read(size - pos[f])
        pos[f] += len(chunk); buf[f] += chunk; seen = time.time()
        while b'\n' in buf[f]:
            line, buf[f] = buf[f].split(b'\n', 1)
            try: rec = json.loads(line)
            except Exception: continue
            ts = rec.get('timestamp')
            if not ts: 
                rows.append((i, seen, None, rec.get('type'), '')); continue
            t = datetime.datetime.fromisoformat(ts.replace('Z', '+00:00')).timestamp()
            c = (rec.get('message') or {}).get('content')
            kinds = ','.join(b.get('type', '?') for b in c if isinstance(b, dict)) if isinstance(c, list) else ('string' if isinstance(c, str) else '')
            rows.append((i, seen, seen - t, rec.get('type'), kinds))
    time.sleep(0.1)
out = ['%s: %d records in %.0fs' % (label, len(rows), secs)]
lags = sorted(r[2] for r in rows if r[2] is not None and r[3] in ('user', 'assistant'))
for i, seen, lag, kind, kinds in rows:
    out.append('file%d %7.2fs lag=%s %-12s %s' % (i, seen - t0, '%.2fs' % lag if lag is not None else '  -  ', kind, kinds))
if lags:
    out.append('user/assistant records: n=%d min=%.2f median=%.2f p90=%.2f max=%.2f' % (
        len(lags), lags[0], lags[len(lags)//2], lags[int(len(lags)*0.9)], lags[-1]))
txt = '\n'.join(out) + '\n'
open(os.path.join(here, 'passive_lag.%s.out.txt' % label), 'w', encoding='utf-8', newline='\n').write(txt)
print(txt)

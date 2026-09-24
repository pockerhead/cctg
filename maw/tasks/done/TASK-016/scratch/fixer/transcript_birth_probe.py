# m2 evidence: birth time of the 60 newest local transcripts vs their first
# timestamped record. Prints (first record type, seconds from birth) counts.
import json, os, glob, datetime
root = os.path.expanduser('~/.claude/projects')
fs = sorted(glob.glob(os.path.join(root, '*', '*.jsonl')), key=os.path.getmtime)[-60:]
out = {}
for f in fs:
    st = os.stat(f); birth = getattr(st, 'st_birthtime', st.st_ctime)
    ts = None; first = None
    for l in open(f, encoding='utf-8', errors='replace'):
        try: r = json.loads(l)
        except Exception: continue
        first = first or r.get('type')
        if r.get('timestamp'): ts = r['timestamp']; break
    if ts:
        d = datetime.datetime.fromisoformat(ts.replace('Z', '+00:00')).timestamp() - birth
        out[(first, round(d))] = out.get((first, round(d)), 0) + 1
for k, v in sorted(out.items(), key=lambda x: -x[1]): print(v, k)

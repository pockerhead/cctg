# Read-only: where does stop_reason == null occur? Aggregates only, no content.
import json, glob, os, collections
root = os.path.expanduser('~/.claude/projects')
files = glob.glob(os.path.join(root, '*', '*.jsonl')) + glob.glob(os.path.join(root, '*', '*', 'subagents', '*.jsonl'))
by_ver = collections.Counter()
pattern = collections.Counter()
null_text_next = collections.Counter()
for f in files:
    sub = 'subagents' in f
    recs = []
    for line in open(f, encoding='utf-8', errors='replace'):
        try: r = json.loads(line)
        except Exception: continue
        if isinstance(r, dict) and r.get('type') in ('user', 'assistant'): recs.append(r)
    msgs = collections.OrderedDict()
    for i, r in enumerate(recs):
        if r['type'] != 'assistant': continue
        m = r.get('message') or {}
        c = m.get('content'); kinds = [b.get('type') for b in c] if isinstance(c, list) else ['<str>']
        sr = m.get('stop_reason', '<absent>')
        if 'text' in kinds and sr is None:
            by_ver[('sub' if sub else 'main', r.get('version'))] += 1
            # what follows this text record (next user/assistant record)?
            nxt = recs[i+1] if i + 1 < len(recs) else None
            if nxt is None: null_text_next[('sub' if sub else 'main', 'EOF')] += 1
            else:
                nm = nxt.get('message') or {}; nc = nm.get('content')
                nk = [b.get('type') for b in nc] if isinstance(nc, list) else ['<str>']
                same = nm.get('id') == m.get('id')
                null_text_next[('sub' if sub else 'main', nxt['type'], ','.join(nk), 'same_msg' if same else 'other_msg', str(nm.get('stop_reason','-')))] += 1
        msgs.setdefault(m.get('id'), []).append(str(sr) + ':' + ','.join(kinds))
    for mid, rows in msgs.items():
        if len(set(x.split(':')[0] for x in rows)) > 1:
            pattern[('sub' if sub else 'main', ' | '.join(rows)[:120])] += 1
print('--- text records with stop_reason null by (scope, version)')
for k, v in sorted(by_ver.items(), key=lambda x: -x[1])[:30]: print(v, k)
print('--- what follows a null-stop text record')
for k, v in sorted(null_text_next.items(), key=lambda x: -x[1])[:30]: print(v, k)
print('--- top stop_reason patterns inside one message.id when they differ')
for k, v in sorted(pattern.items(), key=lambda x: -x[1])[:25]: print(v, k)

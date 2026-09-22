# Read-only: classify user text turns by isMeta and leading tag (tag name only, no content).
import json, glob, os, collections, re
root = os.path.expanduser('~/.claude/projects')
files = glob.glob(os.path.join(root, '*', '*.jsonl'))
c = collections.Counter(); lens = []
for f in files:
    for line in open(f, encoding='utf-8', errors='replace'):
        try: r = json.loads(line)
        except Exception: continue
        if not isinstance(r, dict) or r.get('type') != 'user': continue
        m = r.get('message') or {}; cont = m.get('content')
        texts = [cont] if isinstance(cont, str) else [b.get('text', '') for b in cont if isinstance(b, dict) and b.get('type') == 'text'] if isinstance(cont, list) else []
        for t in texts:
            if not isinstance(t, str): continue
            s = t.lstrip()
            mt = re.match(r'<([a-zA-Z][\w-]*)', s)
            head = '<' + mt.group(1) + '>' if mt else ('[' + s[1:25] + ']' if s.startswith('[') else 'plain')
            c[(bool(r.get('isMeta')), head)] += 1
            lens.append(len(t))
for k, v in sorted(c.items(), key=lambda x: -x[1])[:40]: print(v, k)
lens.sort(); print('user text len p50/p99/max', lens[len(lens)//2], lens[int(len(lens)*.99)], lens[-1])

# Non-tool-result user text records in subagent transcripts: meta flag and leading tag, position.
import json, glob, os, collections, re
ROOT = os.path.expanduser(r'~/.claude/projects')
c = collections.Counter(); first_ok = 0; files = 0
for p in glob.glob(os.path.join(ROOT, '*', '*', 'subagents', 'agent-*.jsonl')):
    files += 1; n = 0
    for l in open(p, encoding='utf-8'):
        if not l.strip(): continue
        r = json.loads(l)
        if r.get('type') != 'user': continue
        m = r.get('message') or {}
        ct = m.get('content')
        texts = [ct] if isinstance(ct, str) else [b.get('text', '') for b in ct if isinstance(b, dict) and b.get('type') == 'text'] if isinstance(ct, list) else []
        for t in texts:
            n += 1
            tag = re.match(r'\s*(<[A-Za-z_-]+|\[Request interrupted)', t)
            c[(('first' if n == 1 else 'later'), bool(r.get('isMeta')), tag.group(1) if tag else 'plain', 'str' if isinstance(ct, str) else 'list')] += 1
print('files', files)
for k, v in c.most_common(30): print(' ', k, v)

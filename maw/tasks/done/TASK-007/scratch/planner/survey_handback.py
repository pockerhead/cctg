# For subagent files with a SubagentHandback tool_use: shape of input, what follows it, final text length.
import json, glob, os, collections
ROOT = os.path.expanduser(r'~/.claude/projects')
jsonls = glob.glob(os.path.join(ROOT, '*', '*', 'subagents', 'agent-*.jsonl'))
input_keys = collections.Counter(); after = collections.Counter(); msg_types = collections.Counter()
hb_count = collections.Counter(); final_len = []; last_user = []
for p in jsonls:
    recs = [json.loads(l) for l in open(p, encoding='utf-8') if l.strip()]
    ua = [r for r in recs if r.get('type') in ('user', 'assistant')]
    idx = []
    for i, r in enumerate(ua):
        c = (r.get('message') or {}).get('content')
        if isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get('type') == 'tool_use' and b.get('name') == 'SubagentHandback':
                    idx.append(i); inp = b.get('input')
                    input_keys[tuple(sorted(inp.keys())) if isinstance(inp, dict) else type(inp).__name__] += 1
                    msg_types[type(inp.get('message')).__name__ if isinstance(inp, dict) else '-'] += 1
    if not idx:
        if ua and ua[-1]['type'] == 'user':
            c = ua[-1]['message'].get('content')
            s = c if isinstance(c, str) else json.dumps(c)[:0]
            if isinstance(c, list):
                s = ' '.join(b.get('type', '') for b in c if isinstance(b, dict))
            last_user.append(s[:40])
        continue
    hb_count[len(idx)] += 1
    tail = ua[idx[-1] + 1:]
    desc = []
    for r in tail:
        c = r['message'].get('content')
        if isinstance(c, list):
            for b in c:
                if isinstance(b, dict):
                    t = b.get('type')
                    if t == 'text' and r['type'] == 'assistant': final_len.append(len(b.get('text', ''))); t = 'text(%d)' % min(len(b.get('text', '')), 999)
                    if t == 'tool_result': t = 'tool_result' + ('/err' if b.get('is_error') else '')
                    desc.append(r['type'][0] + ':' + t)
        else:
            desc.append(r['type'][0] + ':str')
    after[tuple(x if not x.startswith('a:text(') else 'a:text' for x in desc)] += 1
print('handback input keys', dict(input_keys)); print('message types', dict(msg_types)); print('handback calls per file', dict(hb_count))
for k, c in after.most_common(10): print('  after last handback', k, c)
final_len.sort(); print('farewell text len: n', len(final_len), 'median', final_len[len(final_len)//2] if final_len else None, 'max', final_len[-1] if final_len else None)
print('non-handback files ending in user record:', last_user)

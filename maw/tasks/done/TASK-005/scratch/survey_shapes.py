# Shape survey of real transcripts. Prints only structural counts, no content.
import json, glob, os, collections, sys
root = os.path.expanduser(r'~/.claude/projects/C--Users-user-dev-cctg')
files = glob.glob(os.path.join(root, '*.jsonl')) + glob.glob(os.path.join(root, '*', 'subagents', '*.jsonl'))
c = collections.Counter()
keys = collections.Counter()
msgkeys = collections.Counter()
blockkeys = collections.defaultdict(collections.Counter)
msgid_multi = 0; msgid_total = 0
bad = 0
for f in files:
    sub = 'subagents' in f
    ids = collections.Counter()
    for line in open(f, encoding='utf-8'):
        line = line.strip()
        if not line: continue
        try: r = json.loads(line)
        except Exception: bad += 1; continue
        if not isinstance(r, dict): c['nondict'] += 1; continue
        t = r.get('type')
        c[('sub' if sub else 'top', 'type', t)] += 1
        if t not in ('user', 'assistant'):
            if t == 'ai-title': c[('aititle_keys', tuple(sorted(r.keys())))] += 1
            continue
        for k in r.keys(): keys[(t, k)] += 1
        m = r.get('message')
        if not isinstance(m, dict): c[(t, 'message_not_dict')] += 1; continue
        for k in m.keys(): msgkeys[(t, k)] += 1
        ct = m.get('content')
        shape = type(ct).__name__
        c[(t, 'content', shape, 'meta' if r.get('isMeta') else 'nometa', 'side' if r.get('isSidechain') else 'main')] += 1
        if t == 'assistant' and m.get('id'): ids[m['id']] += 1
        if isinstance(ct, list):
            c[(t, 'nblocks', min(len(ct), 3))] += 1
            for b in ct:
                if not isinstance(b, dict): c[(t, 'block_nondict', type(b).__name__)] += 1; continue
                bt = b.get('type')
                c[(t, 'block', bt)] += 1
                for k in b.keys(): blockkeys[(t, bt)][k] += 1
                if bt == 'tool_result':
                    c[('tool_result.content', type(b.get('content')).__name__)] += 1
                    if isinstance(b.get('content'), list):
                        for x in b['content']:
                            c[('tool_result.inner', x.get('type') if isinstance(x, dict) else type(x).__name__)] += 1
                    c[('tool_result.is_error', repr(b.get('is_error')))] += 1
                if bt == 'tool_use':
                    c[('tool_use.input', type(b.get('input')).__name__)] += 1
    for k, v in ids.items():
        msgid_total += 1
        if v > 1: msgid_multi += 1
print('files', len(files), 'bad lines', bad)
for k, v in sorted(c.items(), key=lambda x: str(x[0])): print(k, v)
print('--- record keys'); [print(k, v) for k, v in sorted(keys.items())]
print('--- message keys'); [print(k, v) for k, v in sorted(msgkeys.items())]
print('--- block keys'); [print(k, dict(v)) for k, v in sorted(blockkeys.items(), key=str)]
print('assistant message.id total', msgid_total, 'split across >1 record', msgid_multi)

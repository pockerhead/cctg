# Survey of real subagent artifacts across all projects: .meta.json key/type sets,
# subagent jsonl last-record shapes, SubagentHandback presence, parent linkage.
import json, glob, os, collections
ROOT = os.path.expanduser(r'~/.claude/projects')
metas = glob.glob(os.path.join(ROOT, '*', '*', 'subagents', '*.meta.json'))
jsonls = glob.glob(os.path.join(ROOT, '*', '*', 'subagents', 'agent-*.jsonl'))
print('meta files', len(metas), 'subagent jsonl', len(jsonls))
keys = collections.Counter(); types = collections.Counter(); bad = 0; empty_desc = 0; no_type = 0
for p in metas:
    try:
        m = json.load(open(p, encoding='utf-8'))
    except Exception as e:
        bad += 1; continue
    if not isinstance(m, dict): bad += 1; continue
    for k, v in m.items():
        keys[k] += 1; types[(k, type(v).__name__)] += 1
    if not m.get('description'): empty_desc += 1
    if not m.get('agentType'): no_type += 1
print('meta bad', bad, 'empty description', empty_desc, 'no agentType', no_type)
for k, c in keys.most_common(): print('  key', k, c)
for k, c in sorted(types.items()): print('  type', k, c)
missing_meta = [p for p in jsonls if not os.path.exists(p[:-6] + '.meta.json')]
print('jsonl without meta', len(missing_meta))
handback = 0; handback_last = 0; last_kinds = collections.Counter(); side_false = 0; ends_final = 0
agentid_mismatch = 0; empty_files = 0; compact_prefixed = 0
for p in jsonls:
    aid = os.path.basename(p)[len('agent-'):-len('.jsonl')]
    recs = []
    for l in open(p, encoding='utf-8'):
        l = l.strip()
        if not l: continue
        try: recs.append(json.loads(l))
        except Exception: pass
    if not recs: empty_files += 1; continue
    ua = [r for r in recs if r.get('type') in ('user', 'assistant')]
    if any(r.get('isSidechain') is not True for r in ua): side_false += 1
    if any(r.get('agentId') not in (None, aid) for r in ua): agentid_mismatch += 1
    hb = False
    for r in ua:
        c = (r.get('message') or {}).get('content')
        if isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get('type') == 'tool_use' and b.get('name') == 'SubagentHandback':
                    hb = True
    handback += hb
    last = ua[-1] if ua else None
    if last:
        c = (last.get('message') or {}).get('content')
        kinds = tuple(b.get('type') for b in c if isinstance(b, dict)) if isinstance(c, list) else ('str',)
        last_kinds[(last['type'], kinds, (last.get('message') or {}).get('stop_reason'))] += 1
print('empty files', empty_files, 'files with non-sidechain u/a', side_false, 'agentId mismatch', agentid_mismatch)
print('files with SubagentHandback tool_use', handback)
for k, c in last_kinds.most_common(15): print('  last', k, c)

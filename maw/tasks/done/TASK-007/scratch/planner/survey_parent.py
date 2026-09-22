# Parent (main) transcripts: sidechain records inside them; Agent tool_use input keys; agentId linkage
# between parent toolUseResult.agentId and subagents/agent-<id>.jsonl; meta.toolUseId == Agent tool_use id.
import json, glob, os, collections
ROOT = os.path.expanduser(r'~/.claude/projects')
mains = glob.glob(os.path.join(ROOT, '*', '*.jsonl'))
side_files = 0; side_recs = 0; agent_calls = 0; agent_keys = collections.Counter(); linked = 0; with_file = 0
meta_match = 0; meta_checked = 0; tur_types = collections.Counter(); status = collections.Counter()
for p in mains:
    base = p[:-6]
    sub = os.path.join(base, 'subagents')
    calls = {}; had_side = False
    for l in open(p, encoding='utf-8', errors='replace'):
        if not l.strip(): continue
        try: r = json.loads(l)
        except Exception: continue
        if r.get('type') in ('user', 'assistant') and r.get('isSidechain') is True:
            side_recs += 1; had_side = True
        c = (r.get('message') or {}).get('content') if isinstance(r.get('message'), dict) else None
        if not isinstance(c, list): continue
        for b in c:
            if not isinstance(b, dict): continue
            if b.get('type') == 'tool_use' and b.get('name') in ('Agent', 'Task'):
                agent_calls += 1; inp = b.get('input') or {}
                agent_keys[tuple(sorted(k for k in inp.keys() if k in ('subagent_type','description','prompt','name','run_in_background','model')))] += 1
                calls[b.get('id')] = inp
            if b.get('type') == 'tool_result' and b.get('tool_use_id') in calls:
                tur = r.get('toolUseResult'); tur_types[type(tur).__name__] += 1
                if isinstance(tur, dict):
                    status[tur.get('status')] += 1
                    aid = tur.get('agentId')
                    if aid:
                        linked += 1
                        f = os.path.join(sub, 'agent-%s.jsonl' % aid)
                        if os.path.exists(f):
                            with_file += 1
                            mp = f[:-6] + '.meta.json'
                            if os.path.exists(mp):
                                meta_checked += 1
                                m = json.load(open(mp, encoding='utf-8'))
                                if m.get('toolUseId') == b.get('tool_use_id'): meta_match += 1
    side_files += had_side
print('main files', len(mains), 'with sidechain u/a records', side_files, 'records', side_recs)
print('Agent/Task calls', agent_calls); [print('  input keys', k, c) for k, c in agent_keys.most_common(8)]
print('toolUseResult types on Agent results', dict(tur_types)); print('status', dict(status))
print('results with agentId', linked, 'with subagent file', with_file, 'meta toolUseId == tool_use_id', meta_match, '/', meta_checked)

# meta vs parent Agent input: agentType == subagent_type, description == description
eq_t = eq_d = n = 0; diffs = []
for p in mains:
    calls = {}
    for l in open(p, encoding='utf-8', errors='replace'):
        try: r = json.loads(l)
        except Exception: continue
        c = (r.get('message') or {}).get('content') if isinstance(r.get('message'), dict) else None
        if not isinstance(c, list): continue
        for b in c:
            if isinstance(b, dict) and b.get('type') == 'tool_use' and b.get('name') in ('Agent', 'Task'):
                calls[b.get('id')] = b.get('input') or {}
            if isinstance(b, dict) and b.get('type') == 'tool_result' and b.get('tool_use_id') in calls and isinstance(r.get('toolUseResult'), dict):
                aid = r['toolUseResult'].get('agentId')
                mp = os.path.join(p[:-6], 'subagents', 'agent-%s.meta.json' % aid)
                if aid and os.path.exists(mp):
                    m = json.load(open(mp, encoding='utf-8')); inp = calls[b['tool_use_id']]; n += 1
                    eq_t += m.get('agentType') == inp.get('subagent_type', 'general-purpose')
                    eq_d += m.get('description') == inp.get('description')
                    if m.get('agentType') != inp.get('subagent_type', 'general-purpose'): diffs.append((m.get('agentType'), inp.get('subagent_type')))
print('meta vs Agent input: n', n, 'agentType==subagent_type', eq_t, 'description equal', eq_d, 'type diffs', diffs[:5])

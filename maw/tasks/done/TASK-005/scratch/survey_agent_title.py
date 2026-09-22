# Structural probe: where agent ids live for Agent tool results; ai-title multiplicity per file.
import json, glob, os, collections
root = os.path.expanduser(r'~/.claude/projects/C--Users-user-dev-cctg')
agent_ids = set()
c = collections.Counter()
for f in glob.glob(os.path.join(root, '*.jsonl')):
    titles = []
    agent_tool_ids = set()
    for line in open(f, encoding='utf-8'):
        r = json.loads(line)
        if r.get('type') == 'ai-title': titles.append(r.get('aiTitle'))
        if r.get('type') == 'assistant':
            for b in r['message']['content']:
                if b.get('type') == 'tool_use' and b.get('name') in ('Agent', 'Task'):
                    agent_tool_ids.add(b['id']); c['agent_tool_use:' + b['name']] += 1
                    c['agent_input_keys:' + ','.join(sorted(b['input'].keys()))] += 1
        if r.get('type') == 'user' and isinstance(r['message']['content'], list):
            for b in r['message']['content']:
                if b.get('type') == 'tool_result' and b.get('tool_use_id') in agent_tool_ids:
                    tur = r.get('toolUseResult')
                    c['agent_toolUseResult_type:' + type(tur).__name__] += 1
                    if isinstance(tur, dict): c['agent_toolUseResult_keys:' + ','.join(sorted(tur.keys()))] += 1
                    ct = b.get('content')
                    txt = ct if isinstance(ct, str) else ' '.join(x.get('text', '') for x in ct if isinstance(x, dict))
                    c['agent_result_text_mentions_agentId:' + str('agentId' in txt)] += 1
    c['titles_per_file:%d distinct:%d' % (len(titles), len(set(titles)))] += 1
    if len(set(titles)) > 1: c['first==last:' + str(titles[0] == titles[-1])] += 1
for k, v in sorted(c.items()): print(k, v)

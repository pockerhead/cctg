# Privacy scan (TASK-005 method, extended to .json and to real ids of ALL projects' subagents).
# Run from scratch/planner: python scan_fixtures.py  -> exit 1 on any hit.
import glob, os, re, sys, json
root = os.path.expanduser(r'~/.claude/projects')
real_ids = set(os.path.splitext(os.path.basename(p))[0] for p in glob.glob(os.path.join(root, '*', '*.jsonl')))
real_ids |= set(re.sub(r'^agent-', '', os.path.basename(p).split('.')[0]) for p in glob.glob(os.path.join(root, '*', '*', 'subagents', 'agent-*')))
real_tool_ids = set()
for p in glob.glob(os.path.join(root, '*', '*', 'subagents', '*.meta.json')):
    real_tool_ids.add(json.load(open(p, encoding='utf-8')).get('toolUseId'))
patterns = [r'toolu_0', r'C:\\\\Users', r'Users', r'\.claude', r'<email-user>', r'-100\d{6,}',
            r'\d{8,10}:[A-Za-z0-9_-]{30,}', r'dev\\\\cctg', r'/home/', r'balashow', r'maw-']
hits = 0
files = sorted(glob.glob('fixtures/*.jsonl') + glob.glob('fixtures/*.json'))
for f in files:
    s = open(f, encoding='utf-8').read()
    for p in patterns:
        for m in re.finditer(p, s, re.I):
            print('HIT', f, p, s[max(0, m.start()-30):m.end()+30]); hits += 1
    for i in real_ids | real_tool_ids:
        if i and i in s:
            print('HIT real id', f, i); hits += 1
    for n, line in enumerate(s.splitlines(), 1):
        r = json.loads(line)
        print(f, n, r.get('type'), 'meta' if r.get('isMeta') else '', 'side' if r.get('isSidechain') else '',
              type((r.get('message') or {}).get('content')).__name__ if isinstance(r.get('message'), dict) else '-')
print('files', len(files), 'real ids checked', len(real_ids | real_tool_ids), 'hits', hits)
sys.exit(1 if hits else 0)

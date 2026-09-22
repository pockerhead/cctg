# Privacy scan for fixtures: forbidden substrings and patterns. Exit 1 on any hit.
import glob, os, re, sys, json
root = os.path.expanduser(r'~/.claude/projects/C--Users-user-dev-cctg')
real_ids = set(os.path.splitext(os.path.basename(p))[0] for p in glob.glob(os.path.join(root, '*.jsonl')))
real_ids |= set(re.sub(r'^agent-', '', os.path.splitext(os.path.basename(p))[0]) for p in glob.glob(os.path.join(root, '*', 'subagents', '*.jsonl')))
patterns = [r'toolu_0', r'C:\\Users', r'Users', r'\.claude', r'<email-user>', r'-100\d{6,}', r'\d{8,10}:[A-Za-z0-9_-]{30,}', r'dev\\cctg', r'/home/', r'balashow']
hits = 0
for f in sorted(glob.glob('fixtures/*.jsonl')):
    s = open(f, encoding='utf-8').read()
    for p in patterns:
        for m in re.finditer(p, s, re.I):
            print('HIT', f, p, s[max(0, m.start()-30):m.end()+30]); hits += 1
    for i in real_ids:
        if i in s: print('HIT real id', f, i); hits += 1
    for n, line in enumerate(s.splitlines(), 1):
        r = json.loads(line)
        print(f, n, r.get('type'), 'meta' if r.get('isMeta') else '', 'side' if r.get('isSidechain') else '',
              type((r.get('message') or {}).get('content')).__name__ if isinstance(r.get('message'), dict) else '-')
print('hits', hits); sys.exit(1 if hits else 0)

"""TASK-057: find snippets of the claude bundle around a needle."""
import re, sys, os
data = open(os.path.expanduser('~/.local/bin/claude.exe'), 'rb').read()
needle = sys.argv[1].encode()
width = int(sys.argv[2]) if len(sys.argv) > 2 else 300
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 10
seen = set()
i = 0; n = 0
while n < limit:
    i = data.find(needle, i)
    if i < 0: break
    s = data[max(0, i-width): i+width]
    if s not in seen:
        seen.add(s); n += 1
        print('@%d' % i, s.decode('utf-8', 'replace').replace('\n', ' '))
        print('-----')
    i += len(needle)

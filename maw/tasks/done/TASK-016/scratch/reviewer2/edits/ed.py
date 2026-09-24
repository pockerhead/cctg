# Tiny exact-replace helper: ed.py FILE, then pairs read from a spec module.
import sys
def edit(path, pairs):
    s = open(path, encoding='utf-8').read()
    for a, b in pairs:
        n = s.count(a)
        if n != 1:
            raise SystemExit('%s: %d matches for:\n%s' % (path, n, a[:300]))
        s = s.replace(a, b)
    open(path, 'w', encoding='utf-8', newline='\n').write(s)

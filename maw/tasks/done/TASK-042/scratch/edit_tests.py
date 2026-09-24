# Exact-text replacement helper used to move test spawn sites to tests/common (TASK-042).
base = 'crates/cctg/tests/'
def edit(name, old, new, count=1):
    p = base + name
    s = open(p, encoding='utf-8', newline='').read()
    crlf = '\r\n' in s
    if crlf:
        s = s.replace('\r\n', '\n')
    n = s.count(old)
    assert n == count, (name, n, old[:80])
    s = s.replace(old, new)
    if crlf:
        s = s.replace('\n', '\r\n')
    open(p, 'w', encoding='utf-8', newline='').write(s)

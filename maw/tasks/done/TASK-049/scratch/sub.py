"""Exact-text replacements that keep a file's line endings (CRLF or LF)."""
class Sub:
    def __init__(self, path):
        self.path = path
        self.s = open(path, encoding='utf-8', newline='').read()
        self.crlf = '\r\n' in self.s
    def rep(self, old, new, count=1):
        if self.crlf:
            old = old.replace('\n', '\r\n'); new = new.replace('\n', '\r\n')
        n = self.s.count(old)
        assert n == count, (self.path, old[:80], n)
        self.s = self.s.replace(old, new)
    def save(self):
        open(self.path, 'w', encoding='utf-8', newline='').write(self.s)

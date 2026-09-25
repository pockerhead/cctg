"""Reads cargo's E0063 "missing field `heartbeat`" errors from stdin and adds
the field to each struct literal before its closing brace (TASK-049).

Register -> `heartbeat: false` (old-agent behaviour in existing tests);
HubMsg (the real hub's `registered` answer) -> `heartbeat: true`;
LinkConfig -> `heartbeat: Default::default()`."""
import re, sys
from collections import defaultdict

VALUES = {'Register': 'false', 'HubMsg': 'true', 'LinkConfig': 'Default::default()'}
pat = re.compile(r"missing field `heartbeat` in initializer of `([\w:]+)`\s*-->\s*(\S+):(\d+):(\d+)")
text = sys.stdin.read()
sites = defaultdict(set)
for ty, path, line, col in pat.findall(text):
    sites[path.replace('\\', '/')].add((int(line), int(col), ty.split('::')[-1]))

for path, found in sites.items():
    raw = open(path, encoding='utf-8', newline='').read()
    nl = '\r\n' if '\r\n' in raw else '\n'
    lines = raw.split(nl)
    offsets = [0]
    for l in lines:
        offsets.append(offsets[-1] + len(l) + len(nl))
    inserts = []
    for line, col, ty in found:
        pos = offsets[line - 1] + col - 1
        brace = raw.index('{', pos)
        depth = 0
        i = brace
        while True:
            c = raw[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        body = raw[brace + 1:i]
        value = VALUES[ty]
        if nl in body:
            # Multi-line: a new field line with the indentation of the last one.
            last = body.rstrip().split(nl)[-1]
            indent = last[:len(last) - len(last.lstrip())]
            at = brace + 1 + len(body.rstrip())
            sep = '' if body.rstrip().endswith(',') else ','
            inserts.append((at, f'{sep}{nl}{indent}heartbeat: {value},'))
        else:
            at = brace + 1 + len(body.rstrip())
            sep = '' if body.rstrip().endswith(',') else ','
            inserts.append((at, f'{sep} heartbeat: {value}'))
    for at, s in sorted(inserts, reverse=True):
        raw = raw[:at] + s + raw[at:]
    open(path, 'w', encoding='utf-8', newline='').write(raw)
    print(path, len(found))

# Adds the new TASK-059 wire fields to every construction of HubMsg::Registered
# (albums = files), HubMsg::FileAnswer and AgentMsg::FileOffer (parts: empty).
# Patterns (`..`, `=>`, `} = x`) and AgentEvent::Registered are skipped and
# printed for manual handling. Run once from the repo root, before the type
# edits in wire.rs (the declarations there have `: u64` and are skipped).
import re, glob

def block(s, start):
    depth = 0
    for i in range(start, len(s)):
        if s[i] == '{':
            depth += 1
        elif s[i] == '}':
            depth -= 1
            if depth == 0:
                return i
    raise SystemExit('unbalanced')

changed = {}
for p in glob.glob('crates/**/*.rs', recursive=True):
    s = open(p, encoding='utf-8').read()
    out, pos = [], 0
    for m in re.finditer(r'\b(Registered|FileAnswer|FileOffer) \{', s):
        if m.start() < pos:
            continue
        open_ = m.end() - 1
        end = block(s, open_)
        body = s[open_:end + 1]
        kind = m.group(1)
        line = s.count('\n', 0, m.start()) + 1
        if '..' in body or '\n' not in body or 'to_agent' in body or ': u64' in body or ': bool' in body:
            continue
        after = s[end + 1:end + 12].lstrip()
        if after.startswith('=>') or (after.startswith('=') and not after.startswith('==')):
            print('pattern skipped', p, line)
            continue
        indent = re.search(r'\n([ \t]*)\S', body).group(1)
        if kind == 'Registered':
            f = re.search(r'files: (\w+)', body)
            if not f:
                print('registered without files', p, line)
                continue
            add = f'{indent}albums: {f.group(1)},\n'
        else:
            add = f'{indent}parts: Vec::new(),\n'
        close_line = s.rfind('\n', 0, end) + 1
        prev = s[pos:close_line]
        stripped = prev.rstrip()
        if not stripped.endswith(',') and not stripped.endswith('{'):
            prev = stripped + ',\n'
        out.append(prev + add)
        pos = close_line
        changed[p] = changed.get(p, 0) + 1
    out.append(s[pos:])
    if p in changed:
        open(p, 'w', encoding='utf-8', newline='').write(''.join(out))
print(changed)

# Emulates tests/hub_reads_no_files.rs code_only + identifier scan to find
# which identifier it flags. Usage: python guard_probe.py <file.rs>
import re, sys
src = open(sys.argv[1], encoding='utf-8').read()
cut = src.find('#[cfg(test)]\nmod tests')
src = src if cut < 0 else src[:cut]
ch = list(src)
out = []
i = 0
def char_len(i):
    if i + 1 >= len(ch):
        return None
    if ch[i+1] == '\\':
        for j in range(i+2, min(len(ch), i+14)):
            if ch[j] == "'":
                return j - i + 1
        return None
    return 3 if i + 2 < len(ch) and ch[i+2] == "'" else None
def raw_start(i):
    before = ch[i-1] if i > 0 else None
    ident_before = before is not None and (before.isalnum() or before == '_')
    byte = before == 'b' and (i < 2 or not ch[i-2].isalnum())
    j = i + 1
    while j < len(ch) and ch[j] == '#':
        j += 1
    return (not ident_before or byte) and j < len(ch) and ch[j] == '"'
while i < len(ch):
    c = ch[i]; n = ch[i+1] if i+1 < len(ch) else None
    if c == '/' and n == '/':
        while i < len(ch) and ch[i] != '\n':
            i += 1
    elif c == '"' or (c == 'r' and n in ('"', '#') and raw_start(i)):
        raw = c == 'r'; hashes = 0; i += 1
        while raw and i < len(ch) and ch[i] == '#':
            hashes += 1; i += 1
        i += 1; out.append('"')
        while i < len(ch):
            if not raw and ch[i] == '\\':
                out.append(' '); out.append('\n' if i+1 < len(ch) and ch[i+1] == '\n' else ' '); i += 2; continue
            if ch[i] == '"' and all(i+1+h < len(ch) and ch[i+1+h] == '#' for h in range(hashes)):
                i += 1 + hashes; break
            out.append('\n' if ch[i] == '\n' else ' '); i += 1
        out.append('"')
    elif c == "'" and char_len(i):
        out.append("' '"); i += char_len(i)
    else:
        out.append(c); i += 1
code = ''.join(out)
bad = {'fs','OpenOptions','read_dir','read_to_string','canonicalize','metadata','symlink_metadata','exists','try_exists','is_file','is_dir','is_symlink','read_link','tail','spool','proctree','device'}
for m in re.finditer(r'[A-Za-z0-9_]+', code):
    if m.group(0) in bad:
        line = code[:m.start()].count('\n') + 1
        print(line, m.group(0), repr(code.splitlines()[line-1][:80]))

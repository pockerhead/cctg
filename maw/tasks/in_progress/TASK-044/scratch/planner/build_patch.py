# Rebuilds task044.patch and hashes.txt: the reference workspace against the branch main (not HEAD):
# repo HEAD, both compared with CRLF normalized to LF (the checkout uses
# core.autocrlf=true). The workspace lives outside the repo (a Cargo project
# under scratch/ gets indexed by the IDE and locks the folder, TASK-018).
# Usage: python build_patch.py <workspace root>
import difflib, hashlib, os, subprocess, sys
here = os.path.dirname(os.path.abspath(__file__))
repo = subprocess.check_output(['git', '-C', here, 'rev-parse', '--show-toplevel'], text=True).strip()
ws = sys.argv[1]
CRLF, LF = bytes([13, 10]), bytes([10])
patch, hashes, files = [], [], []
for root in ('CLAUDE.md', 'Cargo.toml', 'Cargo.lock', 'crates', 'docs'):
    p = os.path.join(ws, root)
    if os.path.isfile(p):
        files.append(root)
    for d, _, fs in os.walk(p):
        files += [os.path.relpath(os.path.join(d, f), ws).replace(os.sep, '/') for f in fs]
for f in sorted(set(files)):
    new = open(os.path.join(ws, f), 'rb').read().replace(CRLF, LF)
    r = subprocess.run(['git', '-C', repo, 'show', 'main:' + f], capture_output=True)
    old = r.stdout.replace(CRLF, LF) if r.returncode == 0 else None
    if old == new:
        continue
    a = [] if old is None else old.decode('utf-8').splitlines(keepends=True)
    b = new.decode('utf-8').splitlines(keepends=True)
    patch.append('diff --git a/%s b/%s\n' % (f, f))
    if old is None:
        patch.append('new file mode 100644\n')
    patch += difflib.unified_diff(a, b, '/dev/null' if old is None else 'a/' + f, 'b/' + f, n=3)
    hashes.append('%s  %s\n' % (hashlib.sha256(new).hexdigest(), f))
open(os.path.join(here, 'task044.patch'), 'w', encoding='utf-8', newline='\n').write(''.join(patch))
open(os.path.join(here, 'hashes.txt'), 'w', encoding='utf-8', newline='\n').write(''.join(hashes))
print(''.join(hashes))

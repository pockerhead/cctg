# Rebuilds task015.patch and hashes.txt: ws/ (LF) against the repo HEAD.
import difflib, hashlib, os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
repo = subprocess.check_output(['git', '-C', here, 'rev-parse', '--show-toplevel'], text=True).strip()
ws = os.path.join(here, 'ws')
patch, hashes, files = [], [], []
for root in ('Cargo.toml', 'Cargo.lock', 'crates', 'docs'):
    p = os.path.join(ws, root)
    if os.path.isfile(p):
        files.append(root)
    for d, _, fs in os.walk(p):
        files += [os.path.relpath(os.path.join(d, f), ws).replace(os.sep, '/') for f in fs]
for f in sorted(set(files)):
    new = open(os.path.join(ws, f), 'rb').read()
    r = subprocess.run(['git', '-C', repo, 'show', 'HEAD:' + f], capture_output=True)
    old = r.stdout if r.returncode == 0 else None
    if old == new:
        continue
    a = [] if old is None else old.decode('utf-8').splitlines(keepends=True)
    b = new.decode('utf-8').splitlines(keepends=True)
    patch.append('diff --git a/%s b/%s\n' % (f, f))
    if old is None:
        patch.append('new file mode 100644\n')
    patch += difflib.unified_diff(a, b, '/dev/null' if old is None else 'a/' + f, 'b/' + f, n=3)
    hashes.append('%s  %s\n' % (hashlib.sha256(new).hexdigest(), f))
open(os.path.join(here, 'task015.patch'), 'w', encoding='utf-8', newline='\n').write(''.join(patch))
open(os.path.join(here, 'hashes.txt'), 'w', encoding='utf-8', newline='\n').write(''.join(hashes))
print(''.join(hashes))

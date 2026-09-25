# Rebuilds task031.patch and hashes.txt from the reference workspace.
# Baseline = repo HEAD with TASK-035's scratch/planner/task035.patch applied
# (the workspace's git commit given as the second argument); the patch holds
# only TASK-031's changes. Both sides compared with CRLF normalized to LF (the
# checkout uses core.autocrlf=true). The workspace lives outside the repo (a
# Cargo project under scratch/ gets indexed by the IDE, TASK-018).
# Usage: python build_patch.py <workspace root> <baseline commit in it>
import difflib, hashlib, os, subprocess, sys
here = os.path.dirname(os.path.abspath(__file__))
ws, base = sys.argv[1], sys.argv[2]
CRLF, LF = bytes([13, 10]), bytes([10])
ROOTS = ('CLAUDE.md', 'README.md', 'install.sh', '.gitattributes', 'Cargo.toml', 'Cargo.lock',
         '.gitignore', '.dockerignore', 'Dockerfile', '.github', 'crates', 'deploy', 'docs')
patch, hashes, files = [], [], []
for root in ROOTS:
    p = os.path.join(ws, root)
    if os.path.isfile(p):
        files.append(root)
    for d, _, fs in os.walk(p):
        if os.sep + 'target' in d:
            continue
        files += [os.path.relpath(os.path.join(d, f), ws).replace(os.sep, '/') for f in fs]
for f in sorted(set(files)):
    new = open(os.path.join(ws, f), 'rb').read().replace(CRLF, LF)
    r = subprocess.run(['git', '-C', ws, 'show', base + ':' + f], capture_output=True)
    old = r.stdout.replace(CRLF, LF) if r.returncode == 0 else None
    if old == new:
        continue
    a = [] if old is None else old.decode('utf-8').splitlines(keepends=True)
    b = new.decode('utf-8').splitlines(keepends=True)
    patch.append('diff --git a/%s b/%s\n' % (f, f))
    if old is None:
        patch.append('new file mode %s\n' % ('100755' if f == 'install.sh' else '100644'))
    patch += difflib.unified_diff(a, b, '/dev/null' if old is None else 'a/' + f, 'b/' + f, n=3)
    hashes.append('%s  %s\n' % (hashlib.sha256(new).hexdigest(), f))
open(os.path.join(here, 'task031.patch'), 'w', encoding='utf-8', newline='\n').write(''.join(patch))
open(os.path.join(here, 'hashes.txt'), 'w', encoding='utf-8', newline='\n').write(''.join(hashes))
print(''.join(hashes))

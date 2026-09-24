"""Applies one mutation to the source, runs the given cargo test, restores the file byte for byte.
usage: python mutate.py <name> <file> <old> <new> <cargo test args...>"""
import subprocess, sys, os
name, path, old, new, *args = sys.argv[1:]
src = open(path, 'rb').read()
text = src.decode('utf-8')
assert text.count(old) == 1, (name, text.count(old))
try:
    open(path, 'wb').write(text.replace(old, new).encode('utf-8'))
    r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', *args], capture_output=True, text=True, encoding='utf-8', errors='replace')
    out = r.stdout + r.stderr
    verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
    lines = [l for l in out.splitlines() if ('panicked' in l or 'test result' in l or l.startswith('test ') or 'error' in l)]
    print(f'== {name}: {verdict}')
    print('\n'.join(lines[-15:]))
finally:
    open(path, 'wb').write(src)

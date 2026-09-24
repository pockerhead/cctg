# Time from spawning `cctg agent` to the initialize answer (shim + worker copy).
import os, subprocess, sys, time, tempfile
exe = sys.argv[1]
home = tempfile.mkdtemp(prefix="cctg-fixer-home-")
env = {k: v for k, v in os.environ.items() if not (k.startswith("CCTG_") or k.startswith("CLAUDE"))}
env.update(USERPROFILE=home, HOME=home)
for i in range(3):
    t0 = time.time()
    p = subprocess.Popen([exe, "agent"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, cwd=home)
    p.stdin.write(b'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}\n'); p.stdin.flush()
    line = p.stdout.readline()
    t1 = time.time()
    p.stdin.close(); p.wait()
    print(f"run {i}: initialize answered after {t1-t0:.3f}s")

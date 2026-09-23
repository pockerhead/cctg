"""QA timing/quietness probe for `cctg hook` run the way Claude Code runs a
shell-form hook on Windows: Git Bash `bash -c "cctg hook <Event>"`.
Usage: timing.py <bin> <home> <mode: blackhole|closed|hungstdin> <event> <input> <runs>
Prints only timings, exit codes, stdout size and the fixed stderr lines."""
import os, socket, statistics, subprocess, sys, threading, time

BASH = r"C:\Program Files\Git\bin\bash.exe"
SECRET = "qa-secret-0123456789abcdef"
binp, home, mode, event, inp, runs = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], int(sys.argv[6])
addr_override = sys.argv[7] if len(sys.argv) > 7 else None

held = []
srv = socket.socket(); srv.bind(("127.0.0.1", 0))
port = srv.getsockname()[1]
if mode == "blackhole":
    srv.listen(64)
    def acc():
        while True:
            c, _ = srv.accept(); held.append(c)
    threading.Thread(target=acc, daemon=True).start()
else:
    srv.close()  # closed port: nobody listening

with open(os.path.join(home, ".cctg", "device.env"), "w") as f:
    addr = addr_override or "127.0.0.1:%d" % port
    f.write(f"CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=qabox\n")

env = dict(os.environ); env.pop("CCTG_HUB_SECRET", None)
env["USERPROFILE"] = home; env["HOME"] = home
payload = open(inp, "rb").read()
forbidden = [SECRET, "qa000000", "reason", "dev\\cctg", "dev\\\\cctg", "Users", str(port)]
times, stderrs = [], set()
for _ in range(runs):
    t = time.perf_counter()
    cmd = [BASH, "-c", f"'{binp}' hook {event}"]
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    if mode == "hungstdin":
        out, err = p.communicate(timeout=10) if False else (None, None)
        # keep stdin open and silent until the hook exits
        rc = p.wait(timeout=10)
        out = p.stdout.read(); err = p.stderr.read(); p.stdin.close()
    else:
        out, err = p.communicate(payload, timeout=10); rc = p.returncode
    dt = (time.perf_counter() - t) * 1000
    times.append(dt)
    e = err.decode("utf-8", "replace")
    leak = [x for x in forbidden if x in e]
    stderrs.add(e.strip())
    ansi = "\x1b[" in e
    if rc != 0 or out or leak or ansi:
        print(f"CONTRACT FAIL rc={rc} stdout={len(out)} leak={leak} ansi={ansi}")
print(f"{mode} {event}: runs={runs} min={min(times):.0f}ms median={statistics.median(times):.0f}ms max={max(times):.0f}ms")
for s in stderrs:
    print("  stderr:", s)

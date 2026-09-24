# First-run latency of a fresh copy vs a fresh hard link of cctg.exe (Defender scan cost).
import os, shutil, subprocess, sys, time
exe = sys.argv[1]
d = os.path.dirname(exe)
for kind in ("copy", "link", "copy", "link", "orig"):
    name = os.path.join(d, f"probe-{kind}-{time.time_ns()}.exe")
    t0 = time.time()
    if kind == "copy":
        shutil.copyfile(exe, name)
    elif kind == "link":
        os.link(exe, name)
    else:
        name = exe
    t1 = time.time()
    subprocess.run([name, "--version"], capture_output=True)
    t2 = time.time()
    print(f"{kind}: make {t1-t0:.3f}s, first run {t2-t1:.3f}s")
    if kind != "orig":
        os.remove(name)

"""Median wall time of `cctg statusline` printing its own line (no user
command, no hub secret), inside a git repository and outside one.
Usage: python measure_own_line.py <cctg.exe> <repo dir> <non-repo dir>
Uses a throwaway HOME so the real ~/.cctg, ~/.claude, ~/.claude.json are
never read."""
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time

exe, repo, plain = sys.argv[1:4]
home = tempfile.mkdtemp(prefix="cctg-056-measure-")
env = {k: v for k, v in os.environ.items()
       if not k.startswith(("CCTG_", "CLAUDE"))}
env["HOME"] = env["USERPROFILE"] = home


def once(cwd):
    data = json.dumps({"session_id": "s", "model": {"display_name": "Opus"},
                       "workspace": {"current_dir": cwd},
                       "context_window": {"used_percentage": 12}}).encode()
    t = time.perf_counter()
    out = subprocess.run([exe, "statusline"], input=data, capture_output=True, env=env)
    return time.perf_counter() - t, out.stdout.decode("utf-8", "replace")


for name, cwd in (("repo", repo), ("no repo", plain)):
    once(cwd)
    runs = [once(cwd) for _ in range(20)]
    print(f"{name}: median {statistics.median(r[0] for r in runs) * 1000:.0f} ms,"
          f" max {max(r[0] for r in runs) * 1000:.0f} ms; line {runs[0][1]!r}")

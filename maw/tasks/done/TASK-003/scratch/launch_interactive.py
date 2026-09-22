"""TASK-003 spike (TEMPORARY): start a genuinely interactive `claude` in its own
console with a clean environment, wait for the probe hook to record its
SessionStart, then kill that process tree.

Not production code. Only used to capture scenario E.
Requires the throwaway .claude/settings.json (make_settings.py) to be in place.
"""
import json
import os
import shutil
import subprocess
import sys
import time

SCRATCH = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(SCRATCH, "..", "..", "..", "..", ".."))
OUT = os.path.join(SCRATCH, "capture_E_interactive.jsonl")
CLAUDE_EXE = os.environ.get("CCTG_CLAUDE_EXE") or shutil.which("claude") or "claude.exe"
CREATE_NEW_CONSOLE = 0x00000010

# everything Claude Code exports into its children, so the new session starts
# from an environment that carries no trace of the session spawning it
STRIP = [
    "CLAUDE", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID",
    "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_ENV_FILE", "CLAUDE_PROJECT_DIR",
]

env = {k: v for k, v in os.environ.items() if k not in STRIP}
env["CCTG_PROBE_OUT"] = OUT
env["CCTG_PROBE_SCENARIO"] = "E_interactive"

before = os.path.getsize(OUT) if os.path.exists(OUT) else 0
proc = subprocess.Popen([CLAUDE_EXE], cwd=REPO, env=env, creationflags=CREATE_NEW_CONSOLE)
print("spawned pid", proc.pid, flush=True)

deadline = time.time() + 40
got = False
while time.time() < deadline:
    if os.path.exists(OUT) and os.path.getsize(OUT) > before:
        lines = open(OUT, encoding="utf-8").read().splitlines()
        if any(json.loads(l).get("event") == "SessionStart" for l in lines if l.strip()):
            got = True
            break
    time.sleep(1)

print("SessionStart captured:", got, flush=True)
subprocess.call(["taskkill", "/PID", str(proc.pid), "/T", "/F"])
sys.exit(0 if got else 1)

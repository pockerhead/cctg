"""TASK-004 spike (TEMPORARY): start a genuinely interactive `claude` in its own
console, let the probe channel server talk to it, optionally type a slash command
into that console, then kill the process tree.

Adapted from maw/tasks/done/TASK-003/scratch/launch_interactive.py (that file is
not modified).

Usage:
  python launch_interactive_probe.py <label> <seconds> [--keys "/mcp"] [-- <claude args...>]

Environment passed to claude (and inherited by the probe MCP server):
  CCTG_PROBE_LOG, CCTG_PROBE_NONCE, CCTG_PROBE_AT, CCTG_PROBE_SCENARIO, CCTG_PROBE_PERM
"""
import os
import shutil
import subprocess
import sys
import time

SCRATCH = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(SCRATCH, "..", "..", "..", "..", ".."))
CLAUDE_EXE = os.environ.get("CCTG_CLAUDE_EXE") or shutil.which("claude") or "claude.exe"
CREATE_NEW_CONSOLE = 0x00000010
PROJECTS = os.path.join(os.path.expanduser("~"), ".claude", "projects")


def encoded_cwd(path):
    out = os.path.abspath(path)
    for ch in (":", "\\", "/", " "):
        out = out.replace(ch, "-")
    return out


TYPER = os.path.join(SCRATCH, "type_into_console.py")


def send_keys(pid, keys):
    """Push text straight into that process's console input buffer.

    A separate process is used because AttachConsole requires the caller to have
    no console of its own. Focus is irrelevant, so nothing can leak into the
    orchestrator's terminal.
    """
    for text in keys:
        rc = subprocess.call([sys.executable, TYPER, str(pid), text])
        print("typed %r rc=%s" % (text, rc), flush=True)
        time.sleep(1.5)


def main():
    argv = sys.argv[1:]
    label = argv.pop(0)
    seconds = float(argv.pop(0))
    keys = []
    while argv and argv[0] == "--keys":
        argv.pop(0)
        keys.append(argv.pop(0))
    if argv and argv[0] == "--":
        argv.pop(0)
    claude_args = argv

    proj_dir = os.path.join(PROJECTS, encoded_cwd(REPO))
    before = set(os.listdir(proj_dir)) if os.path.isdir(proj_dir) else set()

    env = dict(os.environ)
    env["CCTG_PROBE_SCENARIO"] = label
    cmd = [CLAUDE_EXE] + claude_args
    print("cmd:", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd, cwd=REPO, env=env, creationflags=CREATE_NEW_CONSOLE)
    print("spawned pid", proc.pid, flush=True)

    if keys:
        time.sleep(12)
        send_keys(proc.pid, keys)

    time.sleep(seconds)
    after = set(os.listdir(proj_dir)) if os.path.isdir(proj_dir) else set()
    new = sorted(after - before)
    print("new transcript files:", new, flush=True)
    subprocess.call(["taskkill", "/PID", str(proc.pid), "/T", "/F"])
    with open(os.path.join(SCRATCH, "interactive_%s_session.txt" % label), "w",
              encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(new) + "\n")


if __name__ == "__main__":
    main()

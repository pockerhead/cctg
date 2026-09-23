"""TASK-004 spike (TEMPORARY): drive one interactive `claude` session on a timeline.

Starts claude in its own console, then at given offsets either types into that
console or dumps its screen buffer, then kills the process tree and reports
which transcript files appeared.

Usage:
  python run_interactive_scenario.py <label> <total_seconds> [--step SEC:type:TEXT]
                                     [--step SEC:shot:NAME] -- <claude args...>

"\\r" inside TEXT means Enter.
"""
import json
import os
import shutil
import subprocess
import sys
import time

SCRATCH = os.path.dirname(os.path.abspath(__file__))
REPO = os.environ.get("CCTG_RUN_CWD") or os.path.abspath(
    os.path.join(SCRATCH, "..", "..", "..", "..", ".."))
CLAUDE_EXE = os.environ.get("CCTG_CLAUDE_EXE") or shutil.which("claude") or "claude.exe"
CREATE_NEW_CONSOLE = 0x00000010
PROJECTS = os.path.join(os.path.expanduser("~"), ".claude", "projects")
TYPER = os.path.join(SCRATCH, "type_into_console.py")
SHOOTER = os.path.join(SCRATCH, "read_console_screen.py")


def utc_now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def write_record(path, lines):
    """Per-run evidence: exact argv, times, nonce, exit status (rewritten as the run advances)."""
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(lines) + "\n")


def encoded_cwd(path):
    out = os.path.abspath(path)
    for ch in (":", "\\", "/", " "):
        out = out.replace(ch, "-")
    return out


def main():
    argv = sys.argv[1:]
    label = argv.pop(0)
    total = float(argv.pop(0))
    steps = []
    while argv and argv[0] == "--step":
        argv.pop(0)
        sec, kind, payload = argv.pop(0).split(":", 2)
        steps.append((float(sec), kind, payload))
    if argv and argv[0] == "--":
        argv.pop(0)
    steps.sort()

    proj_dir = os.path.join(PROJECTS, encoded_cwd(REPO))
    before = set(os.listdir(proj_dir)) if os.path.isdir(proj_dir) else set()

    # CLAUDE_CODE_CHILD_SESSION is inherited from the orchestrating session and
    # makes the new session refuse to save a transcript ("Transcript saving is
    # off — inherited CLAUDE_CODE_CHILD_SESSION marker"), which would destroy the
    # evidence this spike needs. The rest is the TASK-003 strip list.
    env = {k: v for k, v in os.environ.items() if k not in (
        "CLAUDE", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID",
        "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_ENV_FILE", "CLAUDE_PROJECT_DIR")}
    env["CCTG_PROBE_SCENARIO"] = label
    cmd = [CLAUDE_EXE] + argv
    argv_path = os.path.join(SCRATCH, "run_%s_argv.txt" % label)
    record = ["label: %s" % label,
              "argv: %s" % json.dumps(cmd, ensure_ascii=False),
              "cwd: %s" % REPO,
              "probe_nonce: %s" % env.get("CCTG_PROBE_NONCE", "NONONCE"),
              "start_utc: %s" % utc_now()]
    write_record(argv_path, record)
    print("cmd:", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd, cwd=REPO, env=env, creationflags=CREATE_NEW_CONSOLE)
    start = time.time()
    print("spawned pid", proc.pid, flush=True)
    record.append("spawned_pid: %s" % proc.pid)
    write_record(argv_path, record)

    for sec, kind, payload in steps:
        wait = start + sec - time.time()
        if wait > 0:
            time.sleep(wait)
        if kind == "type":
            rc = subprocess.call([sys.executable, TYPER, str(proc.pid), payload])
            print("[%5.1fs] type %r rc=%s" % (time.time() - start, payload, rc), flush=True)
        elif kind == "shot":
            out = os.path.join(SCRATCH, "screen_%s_%s.txt" % (label, payload))
            rc = subprocess.call([sys.executable, SHOOTER, str(proc.pid), out])
            print("[%5.1fs] shot %s rc=%s" % (time.time() - start, out, rc), flush=True)

    wait = start + total - time.time()
    if wait > 0:
        time.sleep(wait)
    after = set(os.listdir(proj_dir)) if os.path.isdir(proj_dir) else set()
    new = sorted(after - before)
    print("new transcript files:", new, flush=True)
    exited_before_kill = proc.poll()
    subprocess.call(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    record += ["end_utc: %s" % utc_now(),
               "exit_status_before_kill: %s" % exited_before_kill,
               "exit_status: %s" % proc.wait()]
    write_record(argv_path, record)
    with open(os.path.join(SCRATCH, "interactive_%s_session.txt" % label), "w",
              encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(new) + "\n")


if __name__ == "__main__":
    main()

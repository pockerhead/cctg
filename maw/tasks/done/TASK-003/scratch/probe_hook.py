"""TASK-003 spike probe hook (TEMPORARY, not production code).

Registered in a throwaway project-scope .claude/settings.json for the duration of
the spike and deleted afterwards. For every hook event it appends one redacted
JSON record to $CCTG_PROBE_OUT (default: scratch/capture_unknown.jsonl) with:
  - the raw hook stdin payload
  - the whitelisted CLAUDE_* env vars as seen by the hook process
  - the full Windows ppid chain of the hook process
It also maintains two pid->session_id maps so the ppid fallback can be tested:
  pidmap_env/<CLAUDE_PID>        <- key taken from the env var
  pidmap_walk/<nearest ancestor> <- key taken from the process tree
Never prints a token: only the whitelist below is read, and absolute home paths
are redacted to ~.
"""

import ctypes
import ctypes.wintypes as wt
import datetime
import json
import os
import sys

BS = chr(92)

SCRATCH = os.path.dirname(os.path.abspath(__file__))

ENV_WHITELIST = [
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CCTG_PROBE_SCENARIO",
]

def _home_variants():
    """Every spelling of the home directory that can appear in a payload."""
    roots = set()
    for cand in (
        os.path.expanduser("~"),
        os.environ.get("USERPROFILE"),
        (os.environ.get("HOMEDRIVE", "") + os.environ.get("HOMEPATH", "")) or None,
        os.environ.get("HOME"),
    ):
        if not cand:
            continue
        win = cand.replace("/", BS)
        if len(win) > 3 and win[1:3] == ":" + BS:
            roots.add(win)                                 # C:\Users\name
            roots.add(win.replace(BS, "/"))                # C:/Users/name
            roots.add(win.replace(BS, BS + BS))            # JSON-escaped form
            roots.add("/" + win[0].lower() + win[2:].replace(BS, "/"))  # msys /c/Users/name
        else:
            roots.add(cand)
    # the encoded cwd Claude Code puts in transcript paths still carries the user name
    encoded = set()
    for r in list(roots):
        if len(r) > 3 and r[1:3] in (":" + BS, ":/"):
            enc = r
            for ch in (":", BS, "/", " "):
                enc = enc.replace(ch, "-")
            encoded.add(enc)
    roots |= encoded
    # longest first so a short spelling never shadows a longer one
    return sorted((r for r in roots if len(r) > 3), key=len, reverse=True)


REDACT = _home_variants()


def redact(text):
    for needle in REDACT:
        text = text.replace(needle, "~")
    return text


# --- ppid chain via CreateToolhelp32Snapshot ------------------------------
TH32CS_SNAPPROCESS = 0x00000002


class PROCESSENTRY32(ctypes.Structure):
    _fields_ = [
        ("dwSize", wt.DWORD),
        ("cntUsage", wt.DWORD),
        ("th32ProcessID", wt.DWORD),
        ("th32DefaultHeapID", ctypes.POINTER(ctypes.c_ulong)),
        ("th32ModuleID", wt.DWORD),
        ("cntThreads", wt.DWORD),
        ("th32ParentProcessID", wt.DWORD),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", wt.DWORD),
        ("szExeFile", ctypes.c_char * 260),
    ]


def snapshot():
    k32 = ctypes.windll.kernel32
    snap = k32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    table = {}
    try:
        entry = PROCESSENTRY32()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32)
        ok = k32.Process32First(snap, ctypes.byref(entry))
        while ok:
            table[int(entry.th32ProcessID)] = (
                int(entry.th32ParentProcessID),
                entry.szExeFile.decode("latin-1"),
            )
            ok = k32.Process32Next(snap, ctypes.byref(entry))
    finally:
        k32.CloseHandle(snap)
    return table


def ppid_chain():
    """Walk up the process tree. Returns (chain, unresolved_parent_pid).

    unresolved_parent_pid is the ParentProcessId the walk could not resolve,
    i.e. that process had already exited -> the chain is truncated there.
    None means the walk reached a real root.
    """
    table = snapshot()
    chain = []
    pid = os.getpid()
    seen = set()
    unresolved = None
    while pid and pid in table and pid not in seen and len(chain) < 16:
        seen.add(pid)
        parent, name = table[pid]
        chain.append({"pid": pid, "name": name})
        if parent and parent not in table:
            unresolved = parent
        pid = parent
    return chain, unresolved


def nearest_claude_ancestor(chain):
    """First ancestor above the hook script that looks like a claude process."""
    for item in chain[1:]:
        low = item["name"].lower()
        if "claude" in low or low == "node.exe":
            return item["pid"]
    return None


# --- main ------------------------------------------------------------------
def main():
    event = sys.argv[1] if len(sys.argv) > 1 else "unknown"
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw)
    except Exception:
        payload = {"_unparsed": raw}

    chain, unresolved_parent = ppid_chain()
    env = {k: os.environ.get(k) for k in ENV_WHITELIST}
    claude_env_all = {}
    for name, value in os.environ.items():
        upper = name.upper()
        if not upper.startswith("CLAUDE"):
            continue
        if any(s in upper for s in ("TOKEN", "SECRET", "KEY", "PASSWORD", "SOCKET")):
            claude_env_all[name] = "<redacted len=%d>" % len(value)
        else:
            claude_env_all[name] = value
    stdin_session = payload.get("session_id") if isinstance(payload, dict) else None
    env_session = env.get("CLAUDE_CODE_SESSION_ID")
    near = nearest_claude_ancestor(chain)

    record = {
        "ts": datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
        "event": event,
        "hook_pid": os.getpid(),
        "env": env,
        "claude_env_all": claude_env_all,
        "env_session_equals_stdin_session": (
            None if (stdin_session is None or env_session is None) else stdin_session == env_session
        ),
        "ppid_chain": chain,
        "ppid_chain_truncated_at": unresolved_parent,
        "nearest_claude_ancestor_pid": near,
        "stdin": payload,
    }

    # pid -> session maps for the ppid fallback experiment
    if stdin_session:
        for sub, key in (("pidmap_env", env.get("CLAUDE_PID")), ("pidmap_walk", near)):
            if not key:
                continue
            d = os.path.join(SCRATCH, sub)
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, str(key)), "w", encoding="utf-8") as fh:
                fh.write(json.dumps({"session_id": stdin_session, "event": event}))

    out = os.environ.get("CCTG_PROBE_OUT") or os.path.join(SCRATCH, "capture_unknown.jsonl")
    line = redact(json.dumps(record, ensure_ascii=False)) + "\n"
    with open(out, "a", encoding="utf-8") as fh:
        fh.write(line)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # never fail the host session
        sys.stderr.write("probe_hook error: %r\n" % (exc,))
    sys.exit(0)

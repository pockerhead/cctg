"""Claude Code statusline: model [effort] br:branch* dir:dir sh:shell ctx:NN%
                           acc:email 5h:NN% 7d:NN%

Python instead of jq/bash: jq does not exist on this machine, and bare-`bash`
resolution is ambiguous across terminals (WSL shim). Absolute paths only.
"""
import json
import os
import subprocess
import sys

GIT = r"C:\Program Files\Git\bin\git.exe"
if not os.path.exists(GIT):
    GIT = "git"

BOLD, CYAN, YELLOW, RED, RESET = "\x1b[1m", "\x1b[36m", "\x1b[33m", "\x1b[31m", "\x1b[0m"


def git(cwd, *args):
    try:
        r = subprocess.run(
            [GIT, "-C", cwd, "--no-optional-locks", *args],
            capture_output=True, text=True, timeout=3,
        )
        return r.stdout.strip() if r.returncode == 0 else ""
    except Exception:
        return ""


def parent_shell():
    """Walk the process ancestry (toolhelp snapshot, no deps) to the launching shell."""
    try:
        import ctypes
        import ctypes.wintypes as wt

        class PE32(ctypes.Structure):
            _fields_ = [
                ("dwSize", wt.DWORD), ("cntUsage", wt.DWORD),
                ("th32ProcessID", wt.DWORD),
                ("th32DefaultHeapID", ctypes.POINTER(ctypes.c_ulong)),
                ("th32ModuleID", wt.DWORD), ("cntThreads", wt.DWORD),
                ("th32ParentProcessID", wt.DWORD), ("pcPriClassBase", ctypes.c_long),
                ("dwFlags", wt.DWORD), ("szExeFile", ctypes.c_char * 260),
            ]

        k32 = ctypes.windll.kernel32
        snap = k32.CreateToolhelp32Snapshot(0x2, 0)
        if snap in (-1, 0):
            return None
        pe = PE32()
        pe.dwSize = ctypes.sizeof(PE32)
        procs = {}
        if k32.Process32First(snap, ctypes.byref(pe)):
            while True:
                name = pe.szExeFile.decode(errors="replace").lower()
                procs[pe.th32ProcessID] = (pe.th32ParentProcessID, name)
                if not k32.Process32Next(snap, ctypes.byref(pe)):
                    break
        k32.CloseHandle(snap)

        shells = {
            "powershell.exe": "ps", "pwsh.exe": "pwsh", "cmd.exe": "cmd",
            "bash.exe": "bash", "zsh.exe": "zsh", "nu.exe": "nu", "fish.exe": "fish",
        }
        pid = os.getpid()
        for _ in range(10):
            entry = procs.get(pid)
            if not entry:
                return None
            ppid, _ = entry
            if not ppid or ppid == pid:
                return None
            pname = procs.get(ppid, (0, ""))[1]
            if pname in shells:
                return shells[pname]
            pid = ppid
        return None
    except Exception:
        return None


def account_email():
    try:
        with open(os.path.expanduser("~/.claude.json"), encoding="utf-8") as f:
            cfg = json.load(f)
        return (cfg.get("oauthAccount") or {}).get("emailAddress")
    except Exception:
        return None


raw = sys.stdin.read()
try:
    d = json.loads(raw)
except Exception:
    d = {}

try:
    with open(os.path.expanduser("~/.claude/statusline_last_input.json"), "w", encoding="utf-8") as f:
        f.write(raw)
except Exception:
    pass

model_obj = d.get("model") or {}
model = model_obj.get("display_name") or model_obj.get("id") or "?"
effort = (d.get("effort") or {}).get("level")
cwd = (d.get("workspace") or {}).get("current_dir") or d.get("cwd") or os.getcwd()
dirname = os.path.basename(cwd.rstrip("\\/")) or cwd

branch = git(cwd, "branch", "--show-current")
if branch and git(cwd, "status", "--porcelain"):
    branch += "*"

ctx = (d.get("context_window") or {}).get("used_percentage")
rl5h = ((d.get("rate_limits") or {}).get("five_hour") or {}).get("used_percentage")
rl7d = ((d.get("rate_limits") or {}).get("seven_day") or {}).get("used_percentage")

parts = [f"{BOLD}{model}{RESET}"]
if effort:
    parts.append(f"[{effort}]")
if branch:
    parts.append(f"{CYAN}br:{branch}{RESET}")
parts.append(f"dir:{dirname}")
shell = parent_shell()
if shell:
    parts.append(f"sh:{shell}")
if isinstance(ctx, (int, float)):
    parts.append(f"ctx:{round(ctx)}%")

limit_parts = []
email = account_email()
if email:
    limit_parts.append(f"acc:{email}")
if isinstance(rl5h, (int, float)):
    r = round(rl5h)
    color = RED if r >= 95 else (YELLOW if r >= 80 else "")
    limit_parts.append(f"{color}5h:{r}%{RESET}" if color else f"5h:{r}%")
if isinstance(rl7d, (int, float)):
    r = round(rl7d)
    color = RED if r >= 95 else (YELLOW if r >= 80 else "")
    limit_parts.append(f"{color}7d:{r}%{RESET}" if color else f"7d:{r}%")

lines = [" ".join(parts)]
if limit_parts:
    lines.append(" ".join(limit_parts))

sys.stdout.write("\n".join(lines))

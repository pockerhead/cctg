"""TASK-003 spike: second redaction pass.

probe_hook.py replaces the home directory in its plain spellings, but Claude Code
also embeds an ENCODED cwd in transcript paths (C--Users-<name>-dev-cctg), which
still carries the user name. Replace the encoded home prefix with ~enc.
Idempotent; safe to re-run.
"""
import glob, io, os, sys

home = os.environ.get("USERPROFILE") or os.path.expanduser("~")
encoded_home = home
for ch in (":", "\\", "/", " "):
    encoded_home = encoded_home.replace(ch, "-")

targets = sys.argv[1:] or sorted(glob.glob("capture_*.jsonl"))
for path in targets:
    text = io.open(path, encoding="utf-8").read()
    fixed = text.replace(encoded_home, "~enc")
    if fixed != text:
        io.open(path, "w", encoding="utf-8", newline="\n").write(fixed)
        print("redacted", path)
    else:
        print("clean   ", path)

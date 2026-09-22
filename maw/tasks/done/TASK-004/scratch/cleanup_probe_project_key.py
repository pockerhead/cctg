"""TASK-004 fixer: remove exactly the temp-folder projects key left by the consent test.

Prints only booleans/counts, never other config entries.
Usage: python cleanup_probe_project_key.py <folder path relative to home, forward slashes>
  e.g. python cleanup_probe_project_key.py AppData/Local/Temp/cctg_probe_dir
"""
import json
import os
import sys

KEY = os.path.expanduser("~").replace(os.sep, "/") + "/" + sys.argv[1].strip("/")
path = os.path.expanduser("~/.claude.json")
with open(path, encoding="utf-8") as fh:
    raw = fh.read()
data = json.loads(raw)
same_format = json.dumps(data, indent=2, ensure_ascii=False) == raw.rstrip("\n")
projects = data.get("projects") or {}
if not same_format:
    raise SystemExit("~/.claude.json does not round-trip byte-identically, nothing written")
present = KEY in projects
if present:
    del projects[KEY]
    out = json.dumps(data, indent=2, ensure_ascii=False)
    if raw.endswith("\n"):
        out += "\n"
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(out)
with open(path, encoding="utf-8") as fh:
    after = fh.read()
print("format_roundtrip_identical=%s key_was_present=%s probe_occurrences_after=%d"
      % (same_format, present, after.count("probe")))

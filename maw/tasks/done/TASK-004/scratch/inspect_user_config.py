"""TASK-004: report the MCP-relevant shape of ~/.claude.json without dumping secrets.

Prints only key names and the `probe` entry, never other servers' args/env.
"""
import json
import os
import sys

path = os.path.expanduser("~/.claude.json")
data = json.load(open(path, encoding="utf-8"))
print("file:", path, "bytes:", os.path.getsize(path))
top = data.get("mcpServers") or {}
print("top-level mcpServers keys:", sorted(top.keys()))
if "probe" in top:
    print("probe entry:", json.dumps(top["probe"], ensure_ascii=False))
projects = data.get("projects") or {}
print("projects with own mcpServers:",
      sorted(k for k, v in projects.items() if (v or {}).get("mcpServers")))
target = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()
for cand in {target, os.path.abspath(target), os.path.abspath(target).replace("/", "\\")}:
    if cand in projects:
        p = projects[cand]
        print("project", cand)
        print("  keys:", sorted(p.keys()))
        for k in ("mcpServers", "enabledMcpjsonServers", "disabledMcpjsonServers",
                  "hasTrustDialogAccepted", "mcpContextUris"):
            if k in p:
                print("  %s: %s" % (k, json.dumps(p[k], ensure_ascii=False)[:400]))
        break
else:
    print("project", target, "ABSENT from ~/.claude.json")

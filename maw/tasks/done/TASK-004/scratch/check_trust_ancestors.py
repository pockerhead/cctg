"""TASK-004 fixer: is any ancestor of the consent-test folder already trusted?

Read-only. Prints only presence and hasTrustDialogAccepted for ancestor keys,
with the home prefix replaced by ~.
"""
import json
import os

data = json.load(open(os.path.expanduser("~/.claude.json"), encoding="utf-8"))
projects = data.get("projects") or {}
home = os.path.expanduser("~").replace(os.sep, "/")
for anc in (home[:3], home, home + "/AppData", home + "/AppData/Local", home + "/AppData/Local/Temp"):
    entry = projects.get(anc)
    print("%-26s %-7s hasTrustDialogAccepted=%s" % (
        anc.replace(home, "~"), "present" if entry is not None else "absent",
        (entry or {}).get("hasTrustDialogAccepted")))

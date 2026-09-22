"""TASK-003 spike: generate the THROWAWAY project-scope .claude/settings.json
that registers scratch/probe_hook.py on every hook event of interest.
Delete the generated .claude/settings.json when the spike is done; the script
refuses to overwrite an existing one, so a leftover file is loud rather than silent."""
import io, json, os

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "..", ".."))
PROBE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "probe_hook.py").replace("\\", "/")
PY = os.path.join(os.environ.get("CCTG_PYTHON", "C:/Program Files/Python310"), "python.exe").replace("\\", "/")


def hook(event):
    return {"type": "command", "command": '"%s" "%s" %s' % (PY, PROBE, event), "timeout": 20}


settings = {
    "hooks": {
        "SessionStart": [{"hooks": [hook("SessionStart")]}],
        "SessionEnd": [{"hooks": [hook("SessionEnd")]}],
        "Stop": [{"hooks": [hook("Stop")]}],
        "SubagentStart": [{"hooks": [hook("SubagentStart")]}],
        "SubagentStop": [{"hooks": [hook("SubagentStop")]}],
        "PreToolUse": [{"matcher": "SubagentHandback", "hooks": [hook("PreToolUse_SubagentHandback")]}],
        "PostToolUse": [{"matcher": "SubagentHandback", "hooks": [hook("PostToolUse_SubagentHandback")]}],
    }
}

target = os.path.join(REPO, ".claude", "settings.json")
assert not os.path.exists(target), "refusing to overwrite an existing %s" % target
io.open(target, "w", encoding="utf-8", newline="\n").write(json.dumps(settings, indent=2) + "\n")
print("wrote", target)

"""TASK-003: append-only writer for log.jsonl (BOM-free UTF-8, one object per line)."""
import datetime, io, json, os

LOG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "log.jsonl")
TS = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
BASE = {"ts": TS, "stage": "implementer", "provider": "claude", "model": "opus", "effort": "medium"}

ENTRIES = [
    dict(BASE, kind="dead_end",
         body="Rejected the documented primary nesting signal: hook env CLAUDE_CODE_SESSION_ID "
              "differing from stdin session_id. A nested claude -p overwrites CLAUDE_CODE_SESSION_ID "
              "and CLAUDE_PID with its own values, so the condition was false in all 8 probed "
              "sessions, including the deliberately nested B/C/D runs. CLAUDE_CODE_CHILD_SESSION=1 "
              "and CLAUDECODE=1 are also set for a genuine top-level run, so neither discriminates.",
         refs=["scratch/FINDINGS.md#факт-2", "scratch/capture_D_nested_env_stripped.jsonl",
               "scratch/detect_parent_report.txt"]),
    dict(BASE, kind="decision",
         body="detect_parent resolves the parent from the process tree plus a hub registry "
              "claude_pid -> session_id filled from SessionStart: skip the claude ancestor whose pid "
              "equals env CLAUDE_PID, the next claude ancestor is the parent. Correct in 3 of 4 nested "
              "probes; the miss was an env.exe wrapper truncating the ppid walk. Alternative rejected: "
              "relying on the env-variable comparison alone, which never fires. Kept as a cheap first "
              "check only. Recommend three states (TopLevel / Nested / NestedUnknownParent) instead of "
              "Option<SessionId>, because a truncated chain otherwise reads as top-level and costs an "
              "extra forum topic.",
         refs=["scratch/FINDINGS.md#контракт", "scratch/analyze_detect_parent.py",
               "scratch/detect_parent_report.txt"]),
]

with io.open(LOG, "a", encoding="utf-8", newline="\n") as fh:
    for entry in ENTRIES:
        fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
print("appended", len(ENTRIES))

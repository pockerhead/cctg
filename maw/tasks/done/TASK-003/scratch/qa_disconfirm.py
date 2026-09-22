"""QA (TASK-003) independent probe: look for the counter-example that would
invalidate fact 2 - any real hook record where env CLAUDE_CODE_SESSION_ID
differs from stdin session_id."""
import glob, json, io, collections

mismatch, total, missing_env, missing_pid = [], 0, [], []
events = collections.Counter()
keysets = collections.defaultdict(set)
for path in sorted(glob.glob("capture_*.jsonl")):
    for i, line in enumerate(io.open(path, encoding="utf-8"), 1):
        if not line.strip(): continue
        r = json.loads(line)
        total += 1
        ev = r.get("event"); events[(path, ev)] += 1
        env = r.get("env", {}); sid = r.get("stdin", {}).get("session_id")
        es = env.get("CLAUDE_CODE_SESSION_ID")
        if es is None: missing_env.append((path, i, ev))
        elif sid and es != sid: mismatch.append((path, i, ev, es, sid))
        if env.get("CLAUDE_PID") is None: missing_pid.append((path, i, ev))
        if ev: keysets[ev] |= set(r.get("stdin", {}).keys())

print("records:", total)
print("MISMATCH env vs stdin session id:", mismatch or "NONE")
print("records without CLAUDE_CODE_SESSION_ID in env:", missing_env or "NONE")
print("records without CLAUDE_PID in env:", missing_pid or "NONE")
print()
for (p, e), n in sorted(events.items()): print("  %-42s %-16s %d" % (p, e, n))
print()
for ev in ("SubagentStart", "SubagentStop", "PreToolUse", "PostToolUse"):
    if ev in keysets: print(ev, "stdin keys:", sorted(keysets[ev]))

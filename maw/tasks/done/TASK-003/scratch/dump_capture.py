"""TASK-003 spike: pretty-print a redacted probe capture file."""
import io, json, sys

for path in sys.argv[1:]:
    print("######", path)
    for line in io.open(path, encoding="utf-8"):
        r = json.loads(line)
        print("== %s  hook_pid=%s" % (r["event"], r["hook_pid"]))
        print("   claude_env_all:", json.dumps(r.get("claude_env_all", r["env"]), sort_keys=True))
        print("   env_session==stdin_session:", r["env_session_equals_stdin_session"])
        print("   chain:", " <- ".join("%s:%s" % (c["pid"], c["name"]) for c in r["ppid_chain"]))
        print("   nearest_claude_ancestor:", r["nearest_claude_ancestor_pid"])
        print("   stdin keys:", sorted(r["stdin"].keys()))
        print("   stdin:", json.dumps(r["stdin"], ensure_ascii=False)[:1400])
        print()

"""TASK-003 spike: replay every captured hook event through the proposed
detect_parent() contract and report what it would have returned.

Input: the capture_*.jsonl files produced by probe_hook.py.
This is spike analysis, not production code.
"""
import io, json, glob, sys

CLAUDE_NAMES = ("claude.exe", "claude")


def is_claude(name):
    return name.lower() in CLAUDE_NAMES


def detect_parent(hook_input, env, chain, registry):
    """Return (verdict, parent_session_id, which_rule).

    registry: {claude_pid(str) -> session_id} built from earlier SessionStart
    events, i.e. what `cctg hub` would hold.
    """
    own = hook_input.get("session_id")

    # Rule 1 - documented primary signal: env session id != stdin session id.
    env_sess = env.get("CLAUDE_CODE_SESSION_ID")
    if env_sess and own and env_sess != own:
        return ("nested", registry.get(env.get("CLAUDE_PID"), env_sess), "rule1_env_session_id")

    # Rule 2 - process tree: skip our own claude (pid == env CLAUDE_PID),
    # the next claude ancestor is the parent session's process.
    own_pid = env.get("CLAUDE_PID")
    seen_own = False
    for node in chain:
        if not is_claude(node["name"]):
            continue
        if not seen_own and (own_pid is None or str(node["pid"]) == str(own_pid)):
            seen_own = True
            continue
        parent = registry.get(str(node["pid"]))
        if parent:
            return ("nested", parent, "rule2_ppid_chain")
        return ("nested_unknown_parent", None, "rule2_ppid_chain_unregistered")

    return ("top_level", None, "rule3_no_claude_ancestor")


def main(paths):
    for path in paths:
        print("######", path)
        registry = {}
        for line in io.open(path, encoding="utf-8"):
            r = json.loads(line)
            hi, env, chain = r["stdin"], r["env"], r["ppid_chain"]
            if r["event"] != "SessionStart":
                continue
            verdict, parent, rule = detect_parent(hi, env, chain, registry)
            print("  session %s  claude_pid=%s" % (hi.get("session_id"), env.get("CLAUDE_PID")))
            print("    verdict=%-18s parent=%s  via=%s" % (verdict, parent, rule))
            trunc = (r["ppid_chain_truncated_at"] if "ppid_chain_truncated_at" in r
                     else "n/a (captured before the probe recorded truncation)")
            print("    chain: %s" % " <- ".join("%s:%s" % (c["pid"], c["name"]) for c in chain))
            print("    chain_truncated_at=%s" % trunc)
            # the hub registers the session only after classifying it
            if env.get("CLAUDE_PID") and hi.get("session_id"):
                registry[str(env["CLAUDE_PID"])] = hi["session_id"]
        print()


def synthetic():
    """Replay the contract branches that the live captures never produced.

    Chains are written the way ppid_chain stores them: hook process first,
    ancestors after it.
    """
    OWN, PARENT = "sess-child", "sess-parent"
    hook = {"session_id": OWN}
    nested_chain = [
        {"pid": 11, "name": "python.exe"},
        {"pid": 12, "name": "bash.exe"},
        {"pid": 100, "name": "claude.exe"},   # our own session
        {"pid": 13, "name": "bash.exe"},
        {"pid": 200, "name": "claude.exe"},   # the parent session
    ]
    truncated_chain = nested_chain[:4]        # parent claude cut off by a dead pid
    registry_full = {"200": PARENT}

    cases = [
        ("missing CLAUDE_PID, nested chain",
         hook, {"CLAUDECODE": "1", "CLAUDE_CODE_SESSION_ID": OWN}, nested_chain, registry_full),
        ("claude ancestor present but unregistered",
         hook, {"CLAUDE_CODE_SESSION_ID": OWN, "CLAUDE_PID": "100"}, nested_chain, {}),
        ("stale/reused CLAUDE_PID (matches no chain node)",
         hook, {"CLAUDE_CODE_SESSION_ID": OWN, "CLAUDE_PID": "999"}, nested_chain,
         {"100": OWN, "200": PARENT}),
        ("chain truncated below the parent claude",
         hook, {"CLAUDE_CODE_SESSION_ID": OWN, "CLAUDE_PID": "100"}, truncated_chain, registry_full),
        ("env rule 1 fires (env session != stdin session)",
         hook, {"CLAUDE_CODE_SESSION_ID": PARENT, "CLAUDE_PID": "200"}, nested_chain, registry_full),
    ]

    print("###### synthetic replay (branches the captures never produced)")
    for name, hi, env, chain, registry in cases:
        verdict, parent, rule = detect_parent(hi, env, chain, registry)
        print("  %s" % name)
        print("    verdict=%-22s parent=%-12s via=%s" % (verdict, parent, rule))
    print()


if __name__ == "__main__":
    main(sys.argv[1:] or sorted(glob.glob("capture_[A-Z]*.jsonl")))
    synthetic()

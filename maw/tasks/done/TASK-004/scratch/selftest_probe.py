"""TASK-004: self-test the probe server without Claude Code.

Feeds it initialize / notifications/initialized / tools/list / tools/call /
permission_request / an unknown method and asserts stdout is pure JSON-RPC.
"""
import json
import os
import subprocess
import sys

SCRATCH = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(SCRATCH, "probe_channel_server.py")
LOG = os.path.join(SCRATCH, "probe_log_selftest.jsonl")
if os.path.exists(LOG):
    os.remove(LOG)

env = dict(os.environ, CCTG_PROBE_LOG=LOG, CCTG_PROBE_NONCE="SELFTEST", CCTG_PROBE_SCENARIO="selftest")
p = subprocess.Popen([sys.executable, PROBE], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8")
msgs = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "selftest"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
     "params": {"name": "reply", "arguments": {"text": "hello"}}},
    {"jsonrpc": "2.0", "method": "notifications/claude/channel/permission_request",
     "params": {"request_id": "abcde", "tool_name": "Bash", "description": "echo hi",
                "input_preview": "echo hi"}},
    {"jsonrpc": "2.0", "id": 4, "method": "resources/list"},
]
out, err = p.communicate("\n".join(json.dumps(m) for m in msgs) + "\n", timeout=30)

lines = [l for l in out.splitlines() if l.strip()]
parsed = [json.loads(l) for l in lines]  # raises if stdout is not pure JSON-RPC
print("stdout objects:", len(parsed))
for o in parsed:
    print("  ", o.get("method") or ("id=%s" % o.get("id")),
          "error" if "error" in o else "")

kinds = [json.loads(l)["kind"] for l in open(LOG, encoding="utf-8") if l.strip()]
checks = {
    "initialize answered": any(o.get("id") == 1 and "result" in o for o in parsed),
    "channel capability": any(
        o.get("id") == 1 and set(o["result"]["capabilities"]["experimental"]) ==
        {"claude/channel", "claude/channel/permission"} for o in parsed),
    "inbound sent": any(o.get("method") == "notifications/claude/channel" for o in parsed),
    "tools/list answered": any(o.get("id") == 2 and o["result"]["tools"][0]["name"] == "reply"
                               for o in parsed),
    "tools/call logged": "TOOLS_CALL" in kinds,
    "permission relayed": any(
        o.get("method") == "notifications/claude/channel/permission"
        and o["params"] == {"request_id": "abcde", "behavior": "allow"} for o in parsed),
    "unknown method -32601": any(o.get("id") == 4 and o.get("error", {}).get("code") == -32601
                                 for o in parsed),
    "log file written": len(kinds) > 0,
}
for name, ok in checks.items():
    print(("PASS " if ok else "FAIL ") + name)
sys.exit(0 if all(checks.values()) else 1)

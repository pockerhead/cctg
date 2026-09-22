"""TASK-004 spike (TEMPORARY): minimal stdio JSON-RPC 2.0 "development channel"
MCP server used to observe Claude Code channel lifecycle across launch modes.

Not production code. Deleted / unregistered at the end of the spike.

Contract (see maw/project-context/domains/channel.md):
  - stdout carries ONLY JSON-RPC, one object per line. Every diagnostic goes to
    the capture file (CCTG_PROBE_LOG) and to stderr.
  - initialize    -> capabilities.experimental["claude/channel"] = {}
                     capabilities.experimental["claude/channel/permission"] = {}
                     capabilities.tools = {}
  - tools/list    -> one tool `reply(text)`
  - tools/call    -> logged, returns ok
  - outgoing      -> notifications/claude/channel  {content, meta}
                     notifications/claude/channel/permission {request_id, behavior}
  - incoming      -> notifications/claude/channel/permission_request
  - anything else -> JSON-RPC -32601 method not found

Env knobs:
  CCTG_PROBE_LOG      capture file (default: <scratch>/probe_log_default.jsonl)
  CCTG_PROBE_NONCE    nonce embedded in the inbound message (default: NONONCE)
  CCTG_PROBE_PERM     allow | deny  (verdict sent for permission_request)
  CCTG_PROBE_INBOUND  1 | 0 (send the inbound message at all; default 1)
  CCTG_PROBE_AT       comma-separated seconds after `notifications/initialized`
                      at which an inbound message is sent (default "0,6")
"""
import io
import json
import os
import sys
import threading
import time

SCRATCH = os.path.dirname(os.path.abspath(__file__))
LOG_PATH = os.environ.get("CCTG_PROBE_LOG") or os.path.join(SCRATCH, "probe_log_default.jsonl")
NONCE = os.environ.get("CCTG_PROBE_NONCE", "NONONCE")
PERM_BEHAVIOR = os.environ.get("CCTG_PROBE_PERM", "allow")
SEND_INBOUND = os.environ.get("CCTG_PROBE_INBOUND", "1") != "0"

_log_lock = threading.Lock()
_out_lock = threading.Lock()


def log(kind, **fields):
    rec = {"t": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "pid": os.getpid(), "kind": kind}
    rec.update(fields)
    line = json.dumps(rec, ensure_ascii=False)
    with _log_lock:
        with io.open(LOG_PATH, "a", encoding="utf-8", newline="\n") as fh:
            fh.write(line + "\n")
    sys.stderr.write("[probe] " + line + "\n")
    sys.stderr.flush()


def send(obj):
    line = json.dumps(obj, ensure_ascii=False)
    with _out_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()
    log("out", msg=obj)


def send_inbound(tag):
    if not SEND_INBOUND:
        return
    send({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": "PROBE-INBOUND-%s-%s" % (NONCE, tag),
            "meta": {"probe": "1", "tag": tag},
        },
    })


SCHEDULE = [float(x) for x in os.environ.get("CCTG_PROBE_AT", "0,6").split(",") if x.strip()]


def inbound_schedule():
    start = time.time()
    for delay in SCHEDULE:
        wait = start + delay - time.time()
        if wait > 0:
            time.sleep(wait)
        send_inbound("T%g" % delay)


def main():
    log("start",
        argv=sys.argv,
        cwd=os.getcwd(),
        ppid=os.getppid(),
        log_path=LOG_PATH,
        env={k: os.environ.get(k) for k in (
            "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID",
            "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_PROJECT_DIR", "CCTG_PROBE_SCENARIO")})

    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception as exc:  # noqa: BLE001 - probe, log and keep going
            log("in_unparsed", raw=raw[:2000], error=str(exc))
            continue
        log("in", msg=msg)
        method = msg.get("method")
        mid = msg.get("id")

        if method == "initialize":
            send({
                "jsonrpc": "2.0", "id": mid,
                "result": {
                    "protocolVersion": (msg.get("params") or {}).get("protocolVersion", "2025-06-18"),
                    "capabilities": {
                        "tools": {},
                        "experimental": {
                            "claude/channel": {},
                            "claude/channel/permission": {},
                        },
                    },
                    "serverInfo": {"name": "probe", "version": "0.0.1"},
                    "instructions": "TASK-004 probe channel. When a <channel> message arrives, "
                                    "call the `reply` tool once with a short acknowledgement.",
                },
            })
        elif method == "notifications/initialized":
            threading.Thread(target=inbound_schedule, daemon=True).start()
        elif method == "tools/list":
            send({
                "jsonrpc": "2.0", "id": mid,
                "result": {"tools": [{
                    "name": "reply",
                    "description": "Send a reply back to the probe channel.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    },
                }]},
            })
        elif method == "tools/call":
            log("TOOLS_CALL", params=msg.get("params"))
            send({"jsonrpc": "2.0", "id": mid,
                  "result": {"content": [{"type": "text", "text": "probe received reply"}]}})
        elif method == "notifications/claude/channel/permission_request":
            params = msg.get("params") or {}
            log("PERMISSION_REQUEST", params=params)
            rid = params.get("request_id")
            if rid:
                send({"jsonrpc": "2.0",
                      "method": "notifications/claude/channel/permission",
                      "params": {"request_id": rid, "behavior": PERM_BEHAVIOR}})
        elif method is not None and method.startswith("notifications/"):
            log("notification_ignored", method=method)
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32601, "message": "Method not found: %s" % method}})

    log("stdin_eof")


if __name__ == "__main__":
    main()

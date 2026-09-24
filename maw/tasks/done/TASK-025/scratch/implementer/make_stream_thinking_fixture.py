"""Builds crates/transcript/tests/fixtures/stream_thinking.jsonl (TASK-025).

Record shape copied from a real 2.1.28x assistant record (field names and
nesting; every value anonymized). Run from the repo root.
"""
import json

SESSION = "00000000-0000-4000-8000-000000000000"
n = 0


def base(kind, parent):
    global n
    n += 1
    return {
        "parentUuid": parent,
        "isSidechain": False,
        "type": kind,
        "uuid": f"00000000-0000-4000-8000-{n:012d}",
        "timestamp": f"2026-01-01T00:00:{n:02d}.000Z",
        "userType": "redacted",
        "entrypoint": "redacted",
        "cwd": "C:\work\demo",
        "sessionId": SESSION,
        "version": "2.1.281",
        "gitBranch": "main",
    }


def assistant(msg_id, stop, block, sidechain=False):
    rec = base("assistant", None)
    rec["isSidechain"] = sidechain
    rec["message"] = {
        "model": "claude-opus-5-5",
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "content": [block],
        "container": None,
        "stop_reason": stop,
        "stop_sequence": None,
        "stop_details": None,
        "usage": {"input_tokens": 2, "output_tokens": 50,
                  "output_tokens_details": {"thinking_tokens": 36}},
    }
    rec["apiBlockIndex"] = 0
    rec["requestId"] = "redacted"
    return rec


def user(content):
    rec = base("user", None)
    rec["message"] = {"role": "user", "content": content}
    return rec


def thinking(text):
    return {"type": "thinking", "thinking": text, "signature": "SIGNATURE-REDACTED"}


long_text = ("\u043c\u044b\u0441\u043b\u044c " * 300).strip()  # 1799 chars
records = [
    user("Which cargo processes are running?"),
    assistant("msg_t1", "tool_use", thinking("\n\u041f\u0440\u043e\u0432\u0435\u0440\u044e, \u043a\u0430\u043a\u0438\u0435 \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u044b cargo \u0437\u0430\u043f\u0443\u0449\u0435\u043d\u044b.\n")),
    assistant("msg_t1", "tool_use", {"type": "text", "text": "Sending a status."}),
    assistant("msg_t1", "tool_use", {"type": "tool_use", "id": "toolu_reply1",
                                      "name": "mcp__cctg__reply",
                                      "input": {"text": "Checking cargo."}}),
    user([{"tool_use_id": "toolu_reply1", "type": "tool_result",
           "content": [{"type": "text", "text": "sent"}]}]),
    assistant("msg_t2", "tool_use", thinking("")),
    assistant("msg_t2", "tool_use", {"type": "redacted_thinking", "data": "REDACTED-DATA"}),
    assistant("msg_t2", "tool_use", {"type": "tool_use", "id": "toolu_bash1", "name": "Bash",
                                      "input": {"command": "tasklist", "description": "List cargo processes"}}),
    user([{"tool_use_id": "toolu_bash1", "type": "tool_result", "content": "cargo.exe"}]),
    assistant("msg_t3", "end_turn", thinking(long_text)),
    assistant("msg_side", "end_turn", thinking("SIDECHAIN-THINKING"), sidechain=True),
    assistant("msg_t3", "end_turn", {"type": "text", "text": "Two cargo processes."}),
]
with open("crates/transcript/tests/fixtures/stream_thinking.jsonl", "w", encoding="utf-8", newline="\n") as f:
    for r in records:
        f.write(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n")

"""TASK-004: append-only writer for log.jsonl (BOM-free UTF-8, one object per line)."""
import datetime
import io
import json
import os

LOG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "log.jsonl")
TS = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
BASE = {"ts": TS, "stage": "implementer", "provider": "claude", "model": "opus", "effort": "medium"}

ENTRIES = [
    dict(BASE, kind="dead_end",
         body="Headless `claude -p` was the planned way to observe all four launch modes and it "
              "cannot show the channel at all. With --dangerously-load-development-channels the "
              "probe spawns and its tool appears, but the debug log has no 'Channel notifications "
              "registered' line and no 'skipped' line for any server, so the subsystem is never "
              "initialised; three -p sessions dropped every outbound notifications/claude/channel "
              "(grep PROBE-INBOUND in their transcripts = 0) and never sent a permission_request. "
              "Same for -p --resume. Every mode had to be re-observed in a real interactive console.",
         refs=["scratch/FINDINGS.md#2", "scratch/debug_M1c.log", "scratch/debug_M2.log",
               "scratch/probe_log_M1b.jsonl"]),
    dict(BASE, kind="dead_end",
         body="Driving the detached interactive console with WScript.Shell AppActivate(pid) + "
              "SendKeys failed: AppActivate returned false for the claude.exe pid, so nothing was "
              "typed and the first two interactive runs produced no transcript. Replaced with "
              "AttachConsole(pid) + WriteConsoleInputW from a separate process (type_into_console.py) "
              "and ReadConsoleOutputCharacterW for screen capture (read_console_screen.py), which "
              "also removed the risk of keystrokes landing in the orchestrator's own terminal. "
              "Residual limit: input longer than one screen line is treated as a paste and Enter "
              "does not submit it.",
         refs=["scratch/type_into_console.py", "scratch/read_console_screen.py",
               "scratch/FINDINGS.md#11"]),
    dict(BASE, kind="decision",
         body="Detect a nested `claude -p` registration by env CLAUDE_CODE_ENTRYPOINT: 'cli' for an "
              "interactive session, 'sdk-cli' for -p. A nested run does silently spawn a second "
              "instance of our user-scope server and announces its own CLAUDE_CODE_SESSION_ID "
              "(f26f0436 vs parent 684d8e75), so matching agents by session id alone would hand it "
              "a topic slot. Alternative rejected: relying only on the TASK-003 process-tree walk, "
              "which is correct but costs a ppid walk and truncates behind wrapper processes; the "
              "env check is one string compare and is available to the agent itself. Channel traffic "
              "is not a usable signal because a nested -p never gets channel registration at all.",
         refs=["scratch/FINDINGS.md#7", "scratch/probe_log_N4.jsonl", "scratch/probe_log_M1.jsonl"]),
    dict(BASE, kind="decision",
         body="Hub must treat 'no channel' as a state the agent reports, not something it can infer "
              "from the protocol. Without the flag Claude Code answers an outbound "
              "notifications/claude/channel with nothing at all - no ack, no JSON-RPC error - and "
              "only the debug log says 'Channel notifications skipped: server probe not in "
              "--channels list for this session'. /mcp shows the server as connected with "
              "'Capabilities: tools' in both cases, so the UI cannot be scraped for it either. "
              "Alternative rejected: probing liveness by sending a canary inbound and waiting for a "
              "reply, which is indistinguishable from an idle session (a never-prompted session "
              "receives channel messages but starts no turn).",
         refs=["scratch/FINDINGS.md#8", "scratch/debug_N5.log", "scratch/screen_I4_mcp.txt",
               "scratch/debug_I3.log"]),
]

with io.open(LOG, "a", encoding="utf-8", newline="\n") as fh:
    for entry in ENTRIES:
        fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
print("appended", len(ENTRIES))

"""TASK-004 fixer: pull raw launcher/tool evidence out of the implementer transcript.

The implementer's launcher printed the exact claude argv and the spawned pid to
stdout, and that stdout is preserved verbatim as tool_result text in
transcripts/6-implementer.jsonl. This script extracts only the pieces the review
asked for (argv per run, pre-launch config check, `claude --help` grep, the
AppActivate attempt), pairs every launcher pid with the probe start ppid, and
redacts home paths (redact_home_paths.py) before writing
scratch/evidence_from_transcript.txt.

Usage: python extract_transcript_evidence.py
"""
import glob
import json
import os
import re

import redact_home_paths

SCRATCH = os.path.dirname(os.path.abspath(__file__))
TRANSCRIPT = os.path.join(SCRATCH, "..", "transcripts", "6-implementer.jsonl")
OUT = os.path.join(SCRATCH, "evidence_from_transcript.txt")


def redact(text):
    text = redact_home_paths.redact(text)
    text = "\n".join(ln for ln in text.split("\n") if "�" not in ln)  # cp866 taskkill noise
    return re.sub(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "<redacted-account>", text)


def result_text(block):
    c = block.get("content")
    if isinstance(c, list):
        return "\n".join(x.get("text", "") for x in c if isinstance(x, dict))
    return c or ""


uses, results = {}, {}
with open(TRANSCRIPT, encoding="utf-8") as fh:
    for lineno, raw in enumerate(fh, 1):
        try:
            rec = json.loads(raw)
        except ValueError:
            continue
        content = (rec.get("message") or {}).get("content")
        if not isinstance(content, list):
            continue
        for b in content:
            if b.get("type") == "tool_use" and b.get("name") == "Bash":
                uses[b["id"]] = (lineno, rec.get("timestamp"), b["input"].get("command", ""))
            elif b.get("type") == "tool_result":
                results[b.get("tool_use_id")] = (lineno, result_text(b))

probe_ppid = {}
for path in glob.glob(os.path.join(SCRATCH, "probe_log_*.jsonl")):
    with open(path, encoding="utf-8") as fh:
        first = json.loads(fh.readline())
    probe_ppid[os.path.basename(path)] = (first.get("ppid"), first.get("t"),
                                          (first.get("env") or {}).get("CLAUDE_CODE_SESSION_ID"))

out = ["# Extracted from transcripts/6-implementer.jsonl (raw tool_use / tool_result), redacted.", ""]

out.append("## 1. Interactive launcher runs: label, argv after `--`, spawned pid, probe start ppid")
launch = re.compile(r"run_interactive_scenario\.py\"?\s+(\w+)\s+\d+.*?\s--\s(.*?)(?:\s2>&1|\s*\||\s*;|$)", re.S)
for tid, (lineno, ts, cmd) in sorted(uses.items(), key=lambda kv: kv[1][0]):
    m = launch.search(cmd)
    if not m:
        continue
    label, args = m.group(1), m.group(2).strip()
    res_line, res = results.get(tid, (None, ""))
    printed = re.findall(r"cmd: (.*)", res)
    pid = re.findall(r"spawned pid (\d+)", res)
    pl = probe_ppid.get("probe_log_%s.jsonl" % label)
    out.append("- %s (transcript line %s, tool_use ts %s)" % (label, lineno, ts))
    out.append("    argv after --      : %s" % args)
    out.append("    launcher printed   : %s" % (printed[0].strip() if printed else "<not in captured tail>"))
    out.append("    spawned pid        : %s" % (pid[0] if pid else "<not in captured tail>"))
    if pl:
        out.append("    probe_log start    : ppid=%s t=%s sid=%s" % pl)
out.append("")

for title, needle in (("2. Pre-launch check: consent folder known to claude?", "folder already known to claude"),
                      ("3. `claude --help` grep for the channel flag", "claude --help 2>&1 | grep"),
                      ("4. AppActivate/SendKeys attempts", "ACTIVATE_FAILED")):
    out.append("## " + title)
    for tid, (lineno, ts, cmd) in sorted(uses.items(), key=lambda kv: kv[1][0]):
        res_line, res = results.get(tid, (None, ""))
        if needle in cmd or needle in res:
            out.append("- tool_use line %s ts %s" % (lineno, ts))
            out.append("    command: " + cmd.strip().replace("\n", "\n             ")[:1500])
            out.append("    result (line %s, first 1500 chars):" % res_line)
            out.append("      " + (res.strip() or "<empty output>")[:1500].replace("\n", "\n      "))
    out.append("")

with open(OUT, "w", encoding="utf-8", newline="\n") as fh:
    fh.write(redact("\n".join(out)) + "\n")
print("wrote", os.path.basename(OUT), "lines:", len(out))

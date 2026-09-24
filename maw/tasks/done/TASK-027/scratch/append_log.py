# Appends implementer log entries to log.jsonl (BOM-free UTF-8, one object per line).
import datetime
import json
import sys

LOG = sys.argv[1]
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
base = {"stage": "implementer", "provider": "claude", "model": "opus", "effort": "medium"}
entries = [
    {"kind": "decision",
     "body": "Own markdown->Telegram HTML converter in crates/transcript (markdown.rs) instead of pulldown-cmark: tests/purity.rs pins transcript deps to serde, serde_json, unicode-segmentation; a line-based subset with recursive, always-closed tags is enough.",
     "refs": ["crates/transcript/src/markdown.rs", "crates/transcript/tests/purity.rs"]},
    {"kind": "decision",
     "body": "Op::Send/Op::Stream carry html: Option<String> next to the plain text; the scheduler (not the Transport) handles 400 can't parse entities by dropping html and putting the job back at the head of its lane, so the retry is metered and keeps topic order. Alternative rejected: second sendMessage inside BotApi::execute (unmetered).",
     "refs": ["crates/cctg/src/hub/scheduler.rs:dispatch", "crates/cctg/src/hub/scheduler.rs:merge_lines"]},
    {"kind": "decision",
     "body": "Formatted: Stop answers and agent replies (shared send_text), streamed answers, stream Note text and thinking. Plain: prompts, tool lines, notices, buttons, permission prompts, subagent blocks, /brief and /full (their prompts and tool lines are not model markdown). A merged stream message with one formatted line becomes HTML with the plain lines escaped.",
     "refs": ["crates/cctg/src/hub/slots.rs:stream_chunks", "crates/cctg/src/hub/stream.rs:Step::Send"]},
    {"kind": "dead_end",
     "body": "Extracting real answers from ~/.claude/projects jsonl for the converter fixture was denied by the permission classifier; used the real model-written maw/tasks/done/TASK-004/IMPL_SUMMARY.md from the repo instead.",
     "refs": ["crates/transcript/tests/fixtures/answer_markdown.md"]},
]
with open(LOG, "a", encoding="utf-8", newline="\n") as log:
    for entry in entries:
        log.write(json.dumps({"ts": now, **base, **entry}, ensure_ascii=False) + "\n")
print(now)

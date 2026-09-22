"""QA (TASK-003): append one BOM-free UTF-8 JSON line to log.jsonl. Append-only."""
import io, json, os

LOG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "log.jsonl")

ENTRY = {
    "ts": "2026-09-22T17:25:00Z",
    "stage": "qa",
    "provider": "claude",
    "model": "opus",
    "effort": "medium",
    "kind": "dead_end",
    "body": (
        "Tried to disconfirm the spike's central claim by searching every capture for a real hook "
        "record where env CLAUDE_CODE_SESSION_ID differs from stdin session_id, which would have made "
        "the ppid fallback unnecessary. Scanned all 48 records of all seven capture files with an "
        "independent script, not the author's analyzer: no such record exists; the only two mismatches "
        "are the synthetic self-test line with a made-up session id. The counter-example does not hold, "
        "so the env rule stays rejected. Also ruled out that the truncated ppid chains are an artifact "
        "of the probe's own 16-node walk limit: the longest captured chain is 11 nodes."
    ),
    "refs": [
        "scratch/qa_disconfirm.py",
        "scratch/capture_00_selftest.jsonl",
        "scratch/probe_hook.py",
    ],
}

with io.open(LOG, "a", encoding="utf-8", newline="\n") as fh:
    fh.write(json.dumps(ENTRY, ensure_ascii=False) + "\n")
print("appended 1")

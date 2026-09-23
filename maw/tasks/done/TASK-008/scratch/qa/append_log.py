import json, datetime
path = r"C:/Users/user/dev/cctg/maw/tasks/in_progress/TASK-008/log.jsonl"
ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
entries = [
    {"kind": "decision",
     "body": "QA verified scheduler fairness with a step-driver harness (copy of Scheduler::run inside the test module of a scratch workspace copy) that checks priority/FIFO/bucket invariants at every dispatch, over 120 random mixed-traffic scenarios with 429s. Alternative: black-box ordering assertions on transport calls only, rejected because they cannot see whether an eligible permission prompt was queued at the moment an ordinary message went out. Harness killed 3/3 injected mutations (fairness before permission, no same-topic guard, capacity 7).",
     "refs": ["scratch/qa/ws/crates/cctg/src/hub/scheduler.rs", "scratch/qa/sched_tests.rs.txt"]},
    {"kind": "dead_end",
     "body": "Inline bash heredoc feeding a python patch script failed to parse in the Bash tool (unmatched quote from raw-string content); switched to writing test snippets as files and splicing them with head/cat.",
     "refs": ["scratch/qa/sched_tests.rs.txt"]},
]
with open(path, "a", encoding="utf-8", newline="\n") as f:
    for e in entries:
        rec = {"ts": ts, "stage": "qa", "provider": "claude", "model": "opus", "effort": "medium", **e}
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")
print("ok", ts)

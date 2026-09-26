import datetime, json
LOG = r"C:/Users/user/dev/cctg-056/maw/tasks/in_progress/TASK-056/log.jsonl"
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
e = {"ts": now, "stage": "code-reviewer", "provider": "claude", "model": "opus", "effort": "medium",
     "kind": "decision",
     "body": "Verdict SHIP: .5 rounding divergence from statusline.py, GIT_* env leak and missing CREATE_NO_WINDOW on git kept as minor; alternative NEEDS_WORK rejected because each is either deliberate and documented or shared with statusline.py and not reproduced as a user-visible defect.",
     "refs": ["IMPL_REVIEW.md", "crates/cctg/src/statusline.rs:git", "crates/cctg/src/statusline.rs:percent"]}
with open(LOG, "a", encoding="utf-8", newline="\n") as f:
    f.write(json.dumps(e, ensure_ascii=False) + "\n")

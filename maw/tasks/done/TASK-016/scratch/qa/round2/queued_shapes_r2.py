"""QA round 2: what a message queued DURING a turn becomes in the transcript.
For every queue-operation 'remove' (enqueued mid-turn, then consumed), print the shapes of
the next 3 records; and tabulate all queued_command attachments: prompt type, origin kind,
isMeta, and the first tag name of a string prompt ('<xyz' only, never the text)."""
import json, os, re, collections
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
after_remove = collections.Counter(); qc = collections.Counter(); enq_content = collections.Counter()
TAGNAME = re.compile(r'^\s*<([A-Za-z_-]+)')
def shape(r):
    t = r.get("type")
    if t == "attachment":
        a = r.get("attachment") or {}
        return f"attachment:{a.get('type')}"
    if t == "user":
        c = (r.get("message") or {}).get("content")
        return f"user(isMeta={r.get('isMeta')},origin={(r.get('origin') or {}).get('kind') if isinstance(r.get('origin'),dict) else None},content={type(c).__name__})"
    if t == "queue-operation":
        return f"queue-operation:{r.get('operation')}"
    return str(t)
for dp, dn, fn in os.walk(root):
    if "subagents" in dp: continue
    for f in fn:
        if not f.endswith(".jsonl"): continue
        recs = []
        try:
            for l in open(os.path.join(dp, f), encoding="utf-8", errors="replace"):
                try: recs.append(json.loads(l))
                except Exception: recs.append({})
        except OSError: continue
        for i, r in enumerate(recs):
            if not isinstance(r, dict): continue
            if r.get("type") == "queue-operation" and r.get("operation") == "enqueue":
                c = r.get("content")
                m = TAGNAME.match(c) if isinstance(c, str) else None
                enq_content[(type(c).__name__, m.group(1) if m else None)] += 1
            if r.get("type") == "queue-operation" and r.get("operation") == "remove":
                nxt = tuple(shape(x) for x in recs[i+1:i+4] if isinstance(x, dict))
                after_remove[nxt] += 1
            if r.get("type") == "attachment" and (r.get("attachment") or {}).get("type") == "queued_command":
                a = r["attachment"]; p = a.get("prompt")
                m = TAGNAME.match(p) if isinstance(p, str) else None
                qc[(type(p).__name__, m.group(1) if m else None, r.get("isMeta"),
                    (r.get("origin") or {}).get("kind") if isinstance(r.get("origin"), dict) else None,
                    (a.get("origin") or {}).get("kind") if isinstance(a.get("origin"), dict) else None,
                    a.get("commandMode"))] += 1
print("enqueue content (type, leading tag name) -> count")
for k, n in enq_content.most_common(): print(k, n)
print("\nnext 3 records after queue-operation remove -> count (top 15)")
for k, n in after_remove.most_common(15): print(n, k)
print("\nqueued_command attachments (prompt type, leading tag, isMeta, record origin, attachment origin, commandMode) -> count")
for k, n in qc.most_common(): print(k, n)

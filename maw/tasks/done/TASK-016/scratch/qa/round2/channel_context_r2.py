"""QA round 2: for each cctg channel record (user isMeta / queue-operation), print the
structural context: queue-operation 'operation' values, and the kind of the previous
user/assistant record (was a turn running: last assistant stop_reason). No text, no ids."""
import json, os, re
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
TAG = re.compile(r'^\s*<channel\s[^>]*source="cctg"')
for dp, dn, fn in os.walk(root):
    if "subagents" in dp: continue
    for f in fn:
        if not f.endswith(".jsonl"): continue
        try: lines = open(os.path.join(dp, f), encoding="utf-8", errors="replace").read().splitlines()
        except OSError: continue
        if not any('source=\\"cctg\\"' in l or 'source="cctg"' in l for l in lines): continue
        recs=[]
        for l in lines:
            try: recs.append(json.loads(l))
            except Exception: recs.append({})
        last_asst=None
        print("--- file")
        for i,r in enumerate(recs):
            t=r.get("type")
            if t=="assistant":
                m=r.get("message") or {}
                kinds=[b.get("type") for b in m.get("content") or [] if isinstance(b,dict)]
                last_asst=(m.get("stop_reason"), kinds)
            c=r.get("content") if t=="queue-operation" else ((r.get("message") or {}).get("content") if isinstance(r.get("message"),dict) else None)
            if isinstance(c,str) and TAG.match(c):
                print(f"rec#{i} type={t} op={r.get('operation')} isMeta={r.get('isMeta')} origin={(r.get('origin') or {}).get('kind') if isinstance(r.get('origin'),dict) else None} prev_assistant={last_asst}")
            elif t=="queue-operation":
                print(f"rec#{i} type=queue-operation op={r.get('operation')} (non-channel or empty)")

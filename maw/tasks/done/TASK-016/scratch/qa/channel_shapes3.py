"""origin.kind of queued_command attachments, and whether the prompt of an
origin-bearing one starts with an XML-ish tag (tag name only)."""
import json, os, collections, re
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
c = collections.Counter()
for dirpath, _, names in os.walk(root):
    for name in names:
        if not name.endswith(".jsonl"): continue
        try: fh = open(os.path.join(dirpath, name), encoding="utf-8", errors="replace")
        except OSError: continue
        with fh:
            for line in fh:
                if "queued_command" not in line: continue
                try: rec = json.loads(line)
                except Exception: continue
                att = rec.get("attachment") if isinstance(rec, dict) else None
                if not isinstance(att, dict) or att.get("type") != "queued_command": continue
                o = att.get("origin"); kind = o.get("kind") if isinstance(o, dict) else None
                p = att.get("prompt"); m = re.match(r"\s*<([A-Za-z_-]+)", p) if isinstance(p, str) else None
                c[(kind, att.get("isMeta"), m.group(1) if m else None)] += 1
for k, n in c.most_common(): print(n, k)

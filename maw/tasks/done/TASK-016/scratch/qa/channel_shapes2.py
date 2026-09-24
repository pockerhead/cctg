"""Every record type that holds a real channel opening tag (content starts with
<channel source=...>) anywhere in its top-level string fields, plus origin kinds
and queued_command attachment shapes. Structure only, no text."""
import json, os, collections
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
by_type = collections.Counter(); origins = collections.Counter(); queued = collections.Counter()
def walk(v, path=""):
    if isinstance(v, dict):
        for k, x in v.items():
            yield from walk(x, f"{path}.{k}")
    elif isinstance(v, list):
        for x in v:
            yield from walk(x, f"{path}[]")
    elif isinstance(v, str):
        yield path, v
for dirpath, _, names in os.walk(root):
    for name in names:
        if not name.endswith(".jsonl"): continue
        try:
            fh = open(os.path.join(dirpath, name), encoding="utf-8", errors="replace")
        except OSError: continue
        with fh:
            for line in fh:
                try: rec = json.loads(line)
                except Exception: continue
                if not isinstance(rec, dict): continue
                o = rec.get("origin")
                if isinstance(o, dict): origins[(rec.get("type"), o.get("kind"), rec.get("isMeta"))] += 1
                att = rec.get("attachment")
                if isinstance(att, dict) and att.get("type") == "queued_command":
                    p = att.get("prompt")
                    queued[(type(p).__name__, isinstance(p,str) and p.lstrip().startswith("<channel"), tuple(sorted(att.keys())))] += 1
                if "<channel" not in line: continue
                for path, s in walk(rec):
                    if s.lstrip().startswith("<channel source="):
                        src = s.split('"')[1] if '"' in s[:40] else "?"
                        by_type[(rec.get("type"), rec.get("isMeta"), rec.get("isSidechain"), path, src)] += 1
print("records whose string field STARTS with <channel source=...>:")
for k, n in by_type.most_common(): print(" ", n, k)
print("origin kinds (type, kind, isMeta):")
for k, n in origins.most_common(): print(" ", n, k)
print("queued_command attachments (prompt type, starts with <channel, keys):")
for k, n in queued.most_common(): print(" ", n, k)

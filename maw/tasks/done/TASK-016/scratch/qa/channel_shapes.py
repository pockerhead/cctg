"""Shapes of transcript records that carry a <channel ...> tag, from the
user's real transcripts. Prints only record structure and the tag's source
and attribute NAMES; never message text, paths or ids."""
import json, os, re, collections, sys
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
TAG = re.compile(r'<channel\s+([^>]*)>')
ATTR = re.compile(r'([A-Za-z0-9_]+)=(["\'])(.*?)\2')
shapes = collections.Counter()
files = 0
for dirpath, _, names in os.walk(root):
    for name in names:
        if not name.endswith(".jsonl"):
            continue
        path = os.path.join(dirpath, name)
        hit = False
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    if "<channel" not in line:
                        continue
                    try:
                        rec = json.loads(line)
                    except Exception:
                        shapes[("unparsable",)] += 1
                        continue
                    # where does the tag sit?
                    where = []
                    msg = rec.get("message") or {}
                    content = msg.get("content") if isinstance(msg, dict) else None
                    def tags_in(text):
                        out = []
                        for m in TAG.finditer(text):
                            attrs = dict((k, v) for k, _, v in ATTR.findall(m.group(1)))
                            src = attrs.get("source", "?")
                            has_mid = "message_id" in attrs and attrs["message_id"].isdigit()
                            starts = text.lstrip().startswith("<channel")
                            out.append((src, tuple(sorted(attrs)), has_mid, starts))
                        return out
                    if isinstance(content, str):
                        for t in tags_in(content):
                            where.append(("content:str",) + t)
                    elif isinstance(content, list):
                        for i, b in enumerate(content):
                            if isinstance(b, dict):
                                for key in ("text", "content"):
                                    v = b.get(key)
                                    if isinstance(v, str):
                                        for t in tags_in(v):
                                            where.append((f"content[].{b.get('type')}.{key}",) + t)
                    att = rec.get("attachment")
                    if isinstance(att, dict):
                        for key, v in att.items():
                            if isinstance(v, str):
                                for t in tags_in(v):
                                    where.append((f"attachment.{att.get('type')}.{key}",) + t)
                    if not where:
                        continue  # the tag is only mentioned inside other text (tool io)
                    hit = True
                    origin = rec.get("origin")
                    okind = origin.get("kind") if isinstance(origin, dict) else (type(origin).__name__ if origin is not None else None)
                    for w in where:
                        shapes[(rec.get("type"), rec.get("isMeta"), rec.get("isSidechain"), okind, (msg.get("role") if isinstance(msg, dict) else None)) + w] += 1
        except OSError:
            pass
        files += hit
print("files with channel tags:", files)
for k, n in sorted(shapes.items(), key=lambda kv: -kv[1]):
    print(n, k)

"""QA round 2: shapes of every transcript record that carries a <channel source=...> tag.
Structure only: never prints message text, paths or ids. Scans ~/.claude/projects (main and
subagent jsonl). For each hit prints a shape key and counts, plus whether transcript
stream_events semantics would give a Channel event (source="cctg", digits message_id,
user+isMeta string/text block, or attachment.queued_command with string prompt)."""
import json, os, re, collections, sys
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
TAG = re.compile(r'^\s*<channel\s')
SRC = re.compile(r'source="([^"]*)"')
MID = re.compile(r'message_id="([^"]*)"')
shapes = collections.Counter()
files = 0
def where(v, path=""):
    """yield (jsonpath, string) for strings that start with a channel tag"""
    if isinstance(v, str):
        if TAG.match(v):
            yield path, v
    elif isinstance(v, dict):
        for k, x in v.items():
            yield from where(x, f"{path}.{'<id>' if k.startswith('toolu_') else k}")
    elif isinstance(v, list):
        for i, x in enumerate(v):
            yield from where(x, f"{path}[]")
for dp, dn, fn in os.walk(root):
    for f in fn:
        if not f.endswith(".jsonl"):
            continue
        files += 1
        try:
            fh = open(os.path.join(dp, f), encoding="utf-8", errors="replace")
        except OSError:
            continue
        sub = "subagent" if "subagents" in dp else "main"
        with fh:
            for line in fh:
                if "<channel" not in line:
                    continue
                try:
                    r = json.loads(line.strip().lstrip("\ufeff"))
                except Exception:
                    continue
                if not isinstance(r, dict):
                    continue
                for jp, s in where(r):
                    m = SRC.search(s.split(">", 1)[0])
                    src = m.group(1) if m else "?"
                    src = src if src in ("cctg", "webhook", "plugin:fakechat:fakechat") else "other"
                    mid = MID.search(s.split(">", 1)[0])
                    midk = "digits" if mid and mid.group(1).isdigit() else ("none" if not mid else "nondigit")
                    msg = r.get("message") if isinstance(r.get("message"), dict) else {}
                    content = msg.get("content")
                    ctype = type(content).__name__
                    origin = (r.get("origin") or {}).get("kind") if isinstance(r.get("origin"), dict) else None
                    att = r.get("attachment") if isinstance(r.get("attachment"), dict) else {}
                    key = (sub, r.get("type"), r.get("isMeta"), origin, att.get("type"),
                           type(att.get("prompt")).__name__ if att else None, ctype, jp, src, midk)
                    shapes[key] += 1
print(f"jsonl files scanned: {files}")
print("sub | type | isMeta | origin.kind | attachment.type | att.prompt type | message.content type | json path | source | message_id | count")
for k, n in sorted(shapes.items(), key=lambda x: -x[1]):
    print(" | ".join(str(x) for x in k), "|", n)

# Checks the rule "every UTF-16 unit that is not an ASCII letter or digit
# becomes '-'" against real ~/.claude/projects folders: the cwd of the first
# record of one transcript per folder vs the folder name. Prints counts only.
import json, os, re
root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
total = bad = odd = 0
for name in sorted(os.listdir(root)):
    folder = os.path.join(root, name)
    if not os.path.isdir(folder):
        continue
    cwd = None
    for f in os.listdir(folder):
        if not f.endswith(".jsonl"):
            continue
        with open(os.path.join(folder, f), encoding="utf-8", errors="replace") as fh:
            for line in fh:
                try:
                    rec = json.loads(line)
                except Exception:
                    continue
                if isinstance(rec, dict) and isinstance(rec.get("cwd"), str):
                    cwd = rec["cwd"]
                    break
        if cwd:
            break
    if not cwd:
        continue
    units = cwd.encode("utf-16-le")
    enc = "".join(
        chr(u) if chr(u).isascii() and chr(u).isalnum() else "-"
        for u in (int.from_bytes(units[i:i+2], "little") for i in range(0, len(units), 2))
    )
    total += 1
    if re.search(r"[^A-Za-z0-9:\/ _-]", cwd):
        odd += 1
        print("cwd with other chars, match:", enc == name, "len", len(enc))
    if enc != name:
        bad += 1
        print("mismatch: enc len", len(enc), "folder len", len(name))
print("total", total, "mismatches", bad, "with other chars", odd)

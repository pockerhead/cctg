# Inserts `from_name: None,` into the struct literals rustc reported as
# missing it (E0063 lines from missing_fields.txt). Run from the repo root.
import re
from collections import defaultdict

sites = defaultdict(list)
for line in open("maw/tasks/in_progress/TASK-036/scratch/missing_fields.txt", encoding="utf-8"):
    m = re.match(r"(.+?):(\d+):(\d+): error\[E0063\]", line)
    if m:
        sites[m.group(1).replace("\\", "/")].append((int(m.group(2)), int(m.group(3))))

for path, points in sites.items():
    text = open(path, encoding="utf-8", newline="").read()
    nl = "\r\n" if "\r\n" in text else "\n"
    starts = [0]
    for i, c in enumerate(text):
        if c == "\n":
            starts.append(i + 1)
    offsets = []
    for line, col in points:
        # col is 1-based in chars; lines here are ASCII up to the struct name
        pos = starts[line - 1] + col - 1
        brace = text.index("{", pos)
        depth, i, in_str = 0, brace, False
        while True:
            c = text[i]
            if in_str:
                if c == "\\":
                    i += 1
                elif c == '"':
                    in_str = False
            elif c == '"':
                in_str = True
            elif c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        offsets.append(i)
    for close in sorted(offsets, reverse=True):
        # indent of the closing line + 4
        line_start = text.rfind("\n", 0, close) + 1
        before = text[line_start:close]
        if before.strip() == "":
            indent = before + "    "
            text = text[:line_start] + indent + "from_name: None," + nl + text[line_start:]
        else:
            # single-line literal: `X { a, b }`
            head = text[:close].rstrip()
            sep = " " if head.endswith(",") else ", "
            text = head + sep + "from_name: None " + text[close:]
    open(path, "w", encoding="utf-8", newline="").write(text)
    print(path, len(offsets))

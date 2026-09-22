"""TASK-004: strip the account e-mail from the captured console screens.

The screens are verbatim terminal dumps, so they carry the status-line
`acc:<email>`. Nothing else credential-like was found in the captures
(grep for sk-/Bearer/JWT shapes returned nothing).
"""
import glob
import os
import re

SCRATCH = os.path.dirname(os.path.abspath(__file__))
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")

changed = 0
for path in glob.glob(os.path.join(SCRATCH, "*.txt")) + glob.glob(os.path.join(SCRATCH, "*.log")) \
        + glob.glob(os.path.join(SCRATCH, "*.jsonl")):
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    new = EMAIL.sub("<redacted-account>", text)
    if new != text:
        with open(path, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(new)
        changed += 1
        print("redacted", os.path.basename(path))
print("files changed:", changed)

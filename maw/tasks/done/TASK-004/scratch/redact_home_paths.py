"""TASK-004 fixer: replace every form of the home path in scratch/ captures.

  <drive>:\\<U>sers\\<name>   (raw, JSON-escaped, double-escaped, forward slash) -> ~
  /c/<U>sers/<name>                                                            -> ~
  C--<U>sers-<name>          (encoded cwd of ~/.claude/projects)               -> ~enc

Regexes spell the directory as [U]sers so this file never matches its own scan.
JSON validity of every *.jsonl line is checked before and after.

Usage: python redact_home_paths.py
"""
import glob
import json
import os
import re

SCRATCH = os.path.dirname(os.path.abspath(__file__))
NAME = r"[A-Za-z0-9._-]+?"
END = r"(?=[\\/\s\"'`,;:)\]-]|$)"
PATTERNS = [
    (re.compile(r"[A-Za-z]:(?:\\+|/)[U]sers(?:\\+|/)" + NAME + END, re.I), "~"),
    (re.compile(r"/[cC]/[U]sers/" + NAME + END), "~"),
    (re.compile(r"[A-Za-z]--[U]sers-[A-Za-z0-9_]+(?=-|$|[\s\"\\/])"), "~enc"),
]


def redact(text):
    for rx, repl in PATTERNS:
        text = rx.sub(repl, text)
    return text


def bad_json_lines(text):
    n = 0
    for line in text.splitlines():
        if line.strip():
            try:
                json.loads(line)
            except ValueError:
                n += 1
    return n


def main():
    changed = 0
    for path in sorted(glob.glob(os.path.join(SCRATCH, "*"))):
        name = os.path.basename(path)
        if not os.path.isfile(path) or name.endswith(".py"):
            continue
        with open(path, encoding="utf-8", errors="strict") as fh:
            text = fh.read()
        new = redact(text)
        if new == text:
            continue
        if name.endswith(".jsonl") and bad_json_lines(new) != bad_json_lines(text):
            raise SystemExit("json validity changed in %s, nothing written" % name)
        with open(path, "w", encoding="utf-8", newline="") as fh:
            fh.write(new)
        changed += 1
        print("redacted", name)
    print("files changed:", changed)


if __name__ == "__main__":
    main()

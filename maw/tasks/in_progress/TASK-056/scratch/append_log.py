"""Appends implementer decision entries to log.jsonl (BOM-free UTF-8, one
object per line, real UTC time)."""
import datetime
import json

LOG = r"C:/Users/user/dev/cctg-056/maw/tasks/in_progress/TASK-056/log.jsonl"
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
base = {"ts": now, "stage": "implementer", "provider": "claude", "model": "opus", "effort": "medium"}
entries = [
    {"kind": "decision",
     "body": "git branch and git status run side by side, each under GIT_TIMEOUT 400 ms, git from PATH; alternative: one sequential 3 s budget as statusline.py (too slow for a status line).",
     "refs": ["crates/cctg/src/statusline.rs:git_branch", "scratch/measure_own_line.out.txt"]},
    {"kind": "decision",
     "body": "own_line stays pure (input, cwd, branch, email) and own_output does the IO; alternative: own_line reading git and .claude.json itself (untestable without the real home).",
     "refs": ["crates/cctg/src/statusline.rs:own_line"]},
    {"kind": "decision",
     "body": "CLI tests run cctg with current_dir(home) and GIT_CEILING_DIRECTORIES=home parent so the target dir inside a clone shows no branch; alternative: move test homes to %TEMP% (still may sit in a repo on CI).",
     "refs": ["crates/cctg/tests/statusline_cli.rs:run_input"]},
    {"kind": "dead_end",
     "body": "Python heredoc through the Bash tool collapsed one level of backslash escapes (raw ESC bytes, raw newlines in Rust literals); repaired with a script file written by Write.",
     "refs": ["scratch/fix_escapes.py"]},
]
with open(LOG, "a", encoding="utf-8", newline="\n") as f:
    for e in entries:
        f.write(json.dumps({**base, **e}, ensure_ascii=False) + "\n")

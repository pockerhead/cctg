"""QA TASK-023 mutations: apply one, run a test target, restore byte for byte.

usage: python mutate.py <name> <cargo test args...>
"""
import os
import subprocess
import sys

ROOT = r"C:\Users\user\dev\cctg"
MUTS = {
    # Old behaviour: every answer goes as a plain, ungated send.
    "A_answer_ungated": (
        r"crates\cctg\src\hub\slots.rs",
        "if split.prefer_file || split.chunks.len() > room {",
        "if true || split.prefer_file || split.chunks.len() > room {",
    ),
    # No skip of turn ends whose answer is in the topic.
    "B_no_answered_skip": (
        r"crates\cctg\src\hub\stream.rs",
        ".position(|&known| known == end)",
        ".position(|&known| known == end && false)",
    ),
    # Pair by count (FIFO) like before the fix.
    "C_fifo_pairing": (
        r"crates\cctg\src\hub\stream.rs",
        ".is_some_and(|held| held.end.is_none_or(|known| known <= end))",
        ".is_some_and(|_held| true)",
    ),
    # TASK-016 R11: a read in flight to a gone connection is not asked again.
    "R11": (
        r"crates\cctg\src\hub\slots.rs",
        "&& (now >= sent + READ_TIMEOUT || conn != Some(asked))",
        "&& (now >= sent + READ_TIMEOUT)",
    ),
}


def main():
    name, args = sys.argv[1], sys.argv[2:]
    rel, old, new = MUTS[name]
    path = os.path.join(ROOT, rel)
    with open(path, "rb") as f:
        orig = f.read()
    text = orig.decode("utf-8")
    assert text.count(old) == 1, f"{name}: pattern count {text.count(old)}"
    with open(path, "wb") as f:
        f.write(text.replace(old, new).encode("utf-8"))
    try:
        r = subprocess.run(["cargo", "test", "-j", "1", "-p", "cctg", *args],
                           cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
        out = r.stdout + r.stderr
        fails = [l for l in out.splitlines() if l.startswith("test ") and "FAILED" in l]
        print(f"{name} rc={r.returncode} {'KILLED' if r.returncode else 'SURVIVED'} {fails}")
        if "error[" in out:
            print(out[-3000:])
    finally:
        with open(path, "wb") as f:
            f.write(orig)


if __name__ == "__main__":
    main()

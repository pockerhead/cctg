"""Apply a named mutation, run a cargo command, restore the file byte for byte.
usage: python mutate.py <name> <cargo args...>"""
import subprocess, sys, pathlib
ROOT = pathlib.Path("C:/Users/user/dev/cctg")
MUT = {
    # BUG-1 fix of round 2 removed: answers outside the stream leave no mark
    "no_answered_outside": ("crates/cctg/src/hub/slots.rs", "        live.answered_outside(&held);\n", ""),
    # answers ride Op::Send again (the pre-TASK-023 behaviour): release never streams
    "answers_plain": ("crates/cctg/src/hub/slots.rs",
        "        let held = match self.streams.get_mut(session).filter(|_| streamed) {",
        "        let held = match self.streams.get_mut(session).filter(|_| streamed && false) {"),
    # candidate fix for the QA2 counter-example (not a mutation): keep the
    # answered mark when its turn end is read again; advance/rewind prune it
    "keep_answered_mark": ("crates/cctg/src/hub/stream.rs",
        "            self.answered_ends.swap_remove(at);\n            return None;",
        "            let _ = at;\n            return None;"),
}
name, args = sys.argv[1], sys.argv[2:]
path, old, new = MUT[name]
f = ROOT / path
orig = f.read_bytes()
text = orig.decode()
if "\r\n" in text:
    old, new = old.replace("\n", "\r\n"), new.replace("\n", "\r\n")
assert old in text, f"pattern of {name} not found"
f.write_bytes(text.replace(old, new).encode())
try:
    rc = subprocess.call(["cargo"] + args, cwd=ROOT)
finally:
    f.write_bytes(orig)
print(f"MUTATION {name} cargo rc={rc}")

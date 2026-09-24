"""QA round 2: non-vacuity of the QA2 e2e harness. Each mutation edits one repo
source file, rebuilds the cctg binary + harness (one CARGO_TARGET_DIR), runs the
named QA2 tests, and restores the file byte for byte. Expects CARGO_TARGET_DIR,
CARGO_PROFILE_DEV_DEBUG=0 in env."""
import os, subprocess, sys
repo = "C:/Users/user/dev/cctg"
here = os.path.dirname(os.path.abspath(__file__))
e2e = os.path.join(here, "e2e")
tgt = os.environ["CARGO_TARGET_DIR"]
M = [
 ("ingress drops transcript_chunk again (B1 revert)", "crates/cctg/src/hub/ingress.rs",
  "\n                        | AgentMsg::TranscriptChunk { .. }", "",
  ["qa2_two_slots", "qa2_rotation", "qa2_restart_with_open_calls"]),
 ("scheduler never breaks a topic stream (M2 revert)", "crates/cctg/src/hub/scheduler.rs",
  "    fn break_stream(&mut self, thread_id: i64) {\n        self.broken.insert(thread_id);",
  "    fn break_stream(&mut self, thread_id: i64) {\n        if thread_id != i64::MIN {\n            return;\n        }\n        self.broken.insert(thread_id);",
  ["qa2_refused_single_line_is_not_overtaken"]),
 ("open calls are not persisted with the offset", "crates/cctg/src/hub/slots.rs",
  "            stream.calls = calls;\n", "            let _ = calls;\n",
  ["qa2_restart_with_open_calls"]),
]
only = sys.argv[1:]
for name, rel, old, new, tests in M:
    if only and not any(name.startswith(o) for o in only):
        continue
    path = os.path.join(repo, rel)
    orig = open(path, "rb").read()
    text = orig.decode("utf-8")
    assert text.count(old) == 1, f"{name}: pattern count {text.count(old)}"
    open(path, "wb").write(text.replace(old, new).encode("utf-8"))
    try:
        b = subprocess.run(["cargo", "build", "-j", "1", "--offline", "-p", "cctg", "--bin", "cctg"],
                           cwd=repo, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if b.returncode != 0:
            print(f"{name}: BUILD FAILED\n{b.stderr[-2000:]}", flush=True)
            continue
        results = []
        for t in tests:
            r = subprocess.run(["cargo", "test", "-j", "1", "--offline", "--", "--test-threads=1", t],
                               cwd=e2e, capture_output=True, text=True, encoding="utf-8", errors="replace",
                               env=dict(os.environ, CCTG_BIN=os.path.join(tgt, "debug", "cctg.exe")), timeout=900)
            failed = [l for l in r.stdout.splitlines() if l.startswith("test ") and "FAILED" in l]
            results.append((t, "FAILED" if r.returncode != 0 else "passed", failed))
        killed = all(res == "FAILED" for _, res, _ in results)
        print(f"{name}: {'KILLED' if killed else 'SURVIVED'} {results}", flush=True)
    finally:
        open(path, "wb").write(orig)
subprocess.run(["git", "status", "--short", "--", "crates"], cwd=repo)
print("restored")

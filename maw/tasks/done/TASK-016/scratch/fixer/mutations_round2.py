"""Round-2 fixer non-vacuity check: each mutation undoes one fix; the named
tests must fail. Restores every file afterwards. Run from the repo root with
CARGO_TARGET_DIR set."""
import subprocess, sys

MUTATIONS = [
    ("B1 ingress drops chunks", "crates/cctg/src/hub/ingress.rs",
     "                        | AgentMsg::TranscriptChunk { .. }),\n", "),\n",
     ["--test", "stream_e2e", "e2e_order_partial_line_and_stop_after_lines"]),
    ("M1 None starts at len", "crates/cctg/src/tail.rs",
     "None => last_line_start(&mut file, len),", "None => len,",
     ["--lib", "tail::tests::no_offset_at_a_torn_end"]),
    ("M1 e2e", "crates/cctg/src/tail.rs",
     "None => last_line_start(&mut file, len),", "None => len,",
     ["--test", "stream_e2e", "e2e_resume_at_a_torn_end_does_not_replay_history"]),
    ("M2 every line restarts", "crates/cctg/src/hub/slots.rs",
     "restart: std::mem::take(&mut live.restart),", "restart: true,",
     ["--lib", "hub::slots::tests::lines_after_a_refused_stream_message_never_show_before_it"]),
    ("M2 e2e", "crates/cctg/src/hub/slots.rs",
     "restart: std::mem::take(&mut live.restart),", "restart: true,",
     ["--test", "stream_e2e", "e2e_refused_sends_lose_nothing"]),
    ("M2 4xx breaks too", "crates/cctg/src/hub/scheduler.rs",
     "if (400..500).contains(code)", "if (400..400).contains(code)",
     ["--lib", "hub::scheduler::tests::a_line_telegram_rejects_with_a_4xx"]),
    ("R4 new turn clears claims", "crates/cctg/src/hub/stream.rs",
     "            self.ends_until.get_or_insert(until);\n",
     "            let _ = until;\n            self.ends_unclaimed = 0;\n",
     ["--lib", "hub::slots::tests::a_stop_that_comes_after_the_next_prompt"]),
]

def main():
    results = []
    for name, path, old, new, args in MUTATIONS:
        raw = open(path, "rb").read()
        text = raw.decode("utf-8")
        crlf = "\r\n" in text
        norm = text.replace("\r\n", "\n")
        if norm.count(old) != 1:
            results.append((name, "PATTERN NOT FOUND"))
            continue
        mutated = norm.replace(old, new)
        if crlf:
            mutated = mutated.replace("\n", "\r\n")
        open(path, "wb").write(mutated.encode("utf-8"))
        try:
            run = subprocess.run(["cargo", "test", "-j", "1", "-p", "cctg", *args],
                                 capture_output=True, text=True, encoding="utf-8", errors="replace",
                                 timeout=900)
            ran = "running 0 tests" not in run.stdout and "test result" in run.stdout
            verdict = "KILLED" if run.returncode != 0 and ran else ("SURVIVED" if run.returncode == 0 else f"BUILD/OTHER rc={run.returncode}")
            results.append((name, verdict))
        finally:
            open(path, "wb").write(raw)
    for name, verdict in results:
        print(f"{verdict:10} {name}")
    return 0 if all(v == "KILLED" for _, v in results) else 1

sys.exit(main())

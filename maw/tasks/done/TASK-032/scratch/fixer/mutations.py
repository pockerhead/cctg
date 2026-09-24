"""Fixer self-check: each mutation undoes one fix; its new test must fail.
Run from anywhere; restores every file it touches."""
import os
import subprocess

ROOT = "C:/Users/user/dev/cctg"
SRC = ROOT + "/crates/cctg/src/"
ENV = dict(os.environ, CARGO_TARGET_DIR=ROOT + "/target", CARGO_PROFILE_DEV_DEBUG="0")

MUTATIONS = [
    ("M1 handed to an ended session counts", "hub/slots.rs",
     ".is_some_and(|(_, conn)| conn == fetching.conn);",
     ".is_some_and(|(_, conn)| conn == fetching.conn) || true;",
     "a_file_downloading_when_its_session_ends_stays_in_the_slot"),
    ("M2 overflow drops the file in transit", "hub/buffer.rs",
     "self.messages.remove(usize::from(keep_front));",
     "self.messages.remove(usize::from(keep_front && false));",
     "an_overflow_while_a_file_downloads_drops_the_next_oldest_and_keeps_the_order"),
    ("M3 link losses unbounded", "hub/slots.rs",
     "if losses < MAX_LINK_LOSSES {",
     "if losses < MAX_LINK_LOSSES || true {",
     "a_file_whose_hand_over_is_cut_again_and_again_is_dropped_with_a_notice"),
    ("M4 photo fallback on any 400", "hub/scheduler.rs",
     "Err(error) if error.is_photo_refusal() => {",
     "Err(ApiError::Telegram { code: 400, .. }) => {",
     "only_a_refused_picture_goes_again_as_a_document"),
    ("M5 no Content-Length check", "hub/api.rs",
     ".is_some_and(|length| length > limit)",
     ".is_some_and(|length| length > limit && false)",
     "a_download_stops_at_its_limit"),
    ("M6 no stream size check", "hub/api.rs",
     "if bytes.len() as u64 + piece.len() as u64 > limit {",
     "if bytes.len() as u64 + piece.len() as u64 > limit && false {",
     "a_download_stops_at_its_limit"),
    ("M7 hub events not held behind a save", "agent.rs",
     "if events.is_some() && saving.is_none() =>",
     "if events.is_some() =>",
     "a_file_saved_off_the_loop_still_reaches_claude"),
    ("M8 receiver limit back to the sender chunk", "files.rs",
     "encoded_len(MAX_CHUNK, true).unwrap_or(usize::MAX)",
     "encoded_len(CHUNK, true).unwrap_or(usize::MAX)",
     "an_assembly_takes_only_the_next_piece"),
]

import sys
ONLY = sys.argv[1:]

for name, rel, old, new, test in MUTATIONS:
    if ONLY and name.split()[0] not in ONLY:
        continue
    path = SRC + rel
    original = open(path, "rb").read()
    assert original.count(old.encode()) == 1, (name, old)
    try:
        open(path, "wb").write(original.replace(old.encode(), new.encode()))
        run = subprocess.run(
            ["cargo", "test", "-j", "1", "-p", "cctg", "--lib", "--", test],
            cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=900,
        )
        out = run.stdout + run.stderr
        killed = run.returncode != 0 and "FAILED" in out
        summary = [l for l in out.splitlines() if l.startswith("test result") or "error[" in l]
        print(f"{name}: {'KILLED' if killed else 'SURVIVED'} rc={run.returncode} {summary}")
    except subprocess.TimeoutExpired:
        print(f"{name}: TIMEOUT (counts as killed)")
    finally:
        open(path, "wb").write(original)

"""TASK-034 fixer mutations for items 1-3 (and the guard): each mutation is
applied to a copy-backed source file, the named tests run, the file is
restored. A mutation is killed when its tests fail.

Run from the repo root:
  CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 \
  python maw/tasks/in_progress/TASK-034/scratch/fixer/mutations.py
"""
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[6]

MUTATIONS = [
    (
        "1+2: the agent probes the hub's transcript path",
        "crates/cctg/src/agent.rs",
        "Some(LinkEvent::Message(HubMsg::TranscriptRead { session_id, from, .. })) => {",
        "Some(LinkEvent::Message(HubMsg::TranscriptRead { session_id, from, path })) => {\n"
        "                    let _ = std::fs::canonicalize(&path);",
        ["--lib", "--", "agent::tests::a_transcript_read_is_answered"],
    ),
    (
        "1+2: the cwd guess is frozen instead of the transcript's folder",
        "crates/cctg/src/tail.rs",
        "        if let Some(found) = self.found.get() {\n            return Some(found.clone());\n        }",
        "        if let Some(found) = self.found.get() {\n            return Some(found.clone());\n        }\n"
        "        if let Some(first) = self.by_cwd.first() {\n"
        "            return Some(self.found.get_or_init(|| first.clone()).clone());\n        }",
        ["--lib", "--", "tail::tests::a_new_session", "tail::tests::the_own_folder_is_found"],
    ),
    (
        "1+2: no Claude Code cut for long folder names",
        "crates/cctg/src/tail.rs",
        "    if name.len() <= MAX_FOLDER_NAME {",
        "    if name.len() <= usize::MAX {",
        ["--lib", "--", "tail::tests::claude_codes_folder_names"],
    ),
    (
        "1+2: a link inside the own folder is followed",
        "crates/cctg/src/tail.rs",
        "        if std::fs::canonicalize(&path).ok()? != want {\n            return None;\n        }",
        "",
        ["--lib", "--", "reads::tests::a_junction_out_of_the_session"],
    ),
    (
        "3: a failed calls read is only a miss",
        "crates/cctg/src/hub/slots.rs",
        "                if !passing {\n                    self.open_from_stops(&session);\n                }",
        "",
        ["--lib", "--", "hub::slots::tests::an_agent_that_cannot_give_the_calls"],
    ),
]


def cargo_test(args):
    cmd = ["cargo", "test", "-j", "1", "-p", "cctg", *args]
    run = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
    summary = [line for line in run.stdout.splitlines() if line.startswith("test result")]
    return run.returncode, summary


def main():
    for name, rel, old, new, args in MUTATIONS:
        path = ROOT / rel
        original = path.read_text(encoding="utf-8")
        assert original.count(old) == 1, f"{name}: anchor not found once"
        path.write_text(original.replace(old, new), encoding="utf-8", newline="\n")
        try:
            code, summary = cargo_test(args)
        finally:
            path.write_text(original, encoding="utf-8", newline="\n")
        verdict = "KILLED" if code != 0 else "SURVIVED"
        print(f"{verdict}: {name} :: {' | '.join(summary) or 'no test summary (build failed?)'}")


main()

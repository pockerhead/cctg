#!/usr/bin/env bash
# Mutation probes run by the planner against the reference (TASK-034); the
# results are in mutations.log (every mutant was killed). Each probe edits one
# file, runs the named tests and restores the file with `git checkout`, so run
# it only in a throwaway copy with the reference committed.
# Build env: CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0, -j 1.
set -u
probe() { # name file old new tests...
  local name="$1" file="$2" old="$3" new="$4"; shift 4
  python - "$file" "$old" "$new" <<'PY'
import sys
p, old, new = sys.argv[1:4]
s = open(p, encoding='utf-8').read()
assert old in s, old
open(p, 'w', encoding='utf-8', newline='').write(s.replace(old, new, 1))
PY
  echo "== $name"
  cargo test -j 1 -p cctg "$@" 2>&1 | grep -E "^test |^test result"
  git checkout -q -- "$file"
}
probe "M1 reader ignores session_reads" crates/cctg/src/hub/slots.rs \
  "(bound.session_reads && !bound.leaving && bound.session == session).then_some(conn)" \
  "(!bound.leaving && bound.session == session).then_some(conn)" --test reads_e2e
probe "M2 subagent gate ignores the session id" crates/cctg/src/reads.rs \
  "&& session.file_name().is_some_and(|dir| *dir == *session_id)" "" --lib reads::
probe "M3 a closed link keeps its reads" crates/cctg/src/hub/slots.rs \
  $'            AgentEvent::Disconnected { conn } => {\n                self.fail_reads_of(conn);\n' \
  $'            AgentEvent::Disconnected { conn } => {\n' --test reads_e2e a_read_whose_agent_goes_away
probe "M4 the hub reads the transcript for the title" crates/cctg/src/hub/slots.rs \
  $'    fn read_title(&mut self, session: String, path: String) {\n' \
  $'    fn read_title(&mut self, session: String, path: String) {\n        if let Ok(text) = std::fs::read_to_string(&path) && let Some(title) = transcript::ai_title(&text) { self.registry.set_title(&session, &title); return; }\n' \
  --test hub_reads_no_files

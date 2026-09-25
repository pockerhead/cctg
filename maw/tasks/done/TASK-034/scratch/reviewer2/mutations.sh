#!/usr/bin/env bash
# Mutation probes of plan-reviewer-2 against the fixed reference (TASK-034).
# Results are in mutations.log. Each probe edits one file, runs the named
# tests and restores the file with `git checkout`, so run it only in a
# throwaway copy with the reference applied AND committed.
# Build env: CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0, -j 1.
# A mutant is killed when at least one listed test FAILS (or it does not build).
set -u
probe() { # name file old new cargo-test-args...
  local name="$1" file="$2" old="$3" new="$4"; shift 4
  python - "$file" "$old" "$new" <<'PY'
import sys
p, old, new = sys.argv[1:4]
s = open(p, encoding='utf-8').read()
assert s.count(old) == 1, old
open(p, 'w', encoding='utf-8', newline='').write(s.replace(old, new, 1))
PY
  echo "== $name"
  cargo test -j 1 -p cctg "$@" 2>&1 | grep -E "^test .*FAILED|^test result|^error(\[|:)" | head -8
  git checkout -q -- "$file"
}
S=crates/cctg/src/hub/slots.rs
R=crates/cctg/src/reads.rs

# --- planner's four (still killed) ---
probe "M1 reader ignores session_reads" $S \
  "(bound.session_reads && !bound.leaving && bound.session == session).then_some(conn)" \
  "(!bound.leaving && bound.session == session).then_some(conn)" --test reads_e2e old_agents
probe "M2 subagent gate ignores the session id" $R \
  "&& session.file_name().is_some_and(|dir| *dir == *session_id)" "" --lib -- reads::
probe "M3 a closed link keeps its reads" $S \
  $'            AgentEvent::Disconnected { conn } => {\n                self.fail_reads_of(conn);\n' \
  $'            AgentEvent::Disconnected { conn } => {\n' --test reads_e2e a_read_whose_agent_goes_away
probe "M4 the hub reads the transcript for the title" $S \
  $'    fn read_title(&mut self, session: String, path: String) {\n' \
  $'    fn read_title(&mut self, session: String, path: String) {\n        if let Ok(text) = std::fs::read_to_string(&path) && let Some(title) = transcript::ai_title(&text) { self.registry.set_title(&session, &title); return; }\n' \
  --test hub_reads_no_files

# --- defects fixed by reviewer 2 (both survived the planner's reference) ---
probe "F1 read ids counted per hub run" $S \
  "        let read_id = crate::wire::random_u64();" \
  "        static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let read_id = IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1 + 1;" \
  --lib -- hub::slots::tests::an_answer_left_from_an_earlier_hub_run
probe "F2 the hub probes a file through an aliased fs" $S \
  $'    fn read_title(&mut self, session: String, path: String) {\n' \
  $'    fn read_title(&mut self, session: String, path: String) {\n        { use std::{fs as disk}; if disk::metadata(&path).is_err() { return; } }\n' \
  --test hub_reads_no_files

# --- path gate ---
probe "G1 transcript gate: any depth under the root" crates/cctg/src/tail.rs \
  "let inside = path.parent().and_then(Path::parent) == Some(root.as_path())" \
  "let inside = path.starts_with(&root)" --lib -- tail:: reads::
probe "G2 subagent gate: no root check" $R \
  $'\n        && session.parent().and_then(Path::parent) == Some(root.as_path());' ";" --lib -- reads::
probe "G3 subagent gate: no subagents folder check" $R \
  $'        && subagents.file_name().is_some_and(|dir| dir == "subagents")\n' "" --lib -- reads::
probe "G4 subagent gate: agent id not checked" $R \
  "if !is_agent_id(&input.agent_id) || !is_plain_session_id(session_id) {" \
  "if !is_plain_session_id(session_id) {" --lib -- reads::
probe "G5 transcript gate: session id not checked" crates/cctg/src/tail.rs \
  $'    if !is_plain_session_id(session_id) {\n        return None;\n    }\n    let name' \
  $'    let name' --lib -- tail:: reads::

# --- limits ---
probe "L1 hub takes a text of any length" $S \
  "if text.len() + piece.len() > MAX_READ_TEXT {" "if false {" --lib -- hub::slots::tests::brief_is_rendered
probe "L2 agent sends a text in one piece" $R \
  "let at = if rest.len() <= PIECE {" "let at = if true {" --lib -- reads::
probe "L3 agent sends every Agent call in one answer" $R \
  "        if weight >= MAX_CALLS_WEIGHT {" "        if false {" --lib -- reads::
probe "L4 agent renders any number of prompts" $R \
  "prompts.clamp(1, MAX_PROMPTS)" "prompts.max(1)" --lib -- reads::
probe "L5 a piece does not restart the wait" $S \
  $'            if *more {\n                pending.until = Instant::now() + self.options.read_wait;\n' \
  $'            if *more {\n' --lib -- hub::slots::tests::a_long_answer_keeps_its_read_alive

# --- correlation, timeouts and links ---
probe "C1 any link may answer a read" $S \
  "let Some(pending) = self.reads.get_mut(&read_id).filter(|p| p.conn == conn) else {" \
  "let Some(pending) = self.reads.get_mut(&read_id) else {" --lib -- hub::slots::tests::a_long_answer
probe "C2 no block from the stop without a reader" $S \
  $'            let Some(conn) = self.reader(&session) else {\n                self.open_from_stops(&session);\n' \
  $'            let Some(conn) = self.reader(&session) else {\n' --lib -- hub::slots::tests::without_an_agent_that_reads
probe "C3 a read never times out" $S \
  "            .filter(|(_, pending)| pending.until <= now)" "            .filter(|(_, pending)| pending.until < now && false)" \
  --lib -- hub::slots::tests::a_brief_read_out hub::slots::tests::a_session_read_fails
probe "C4 an agent leaving for an update keeps its reads" $S \
  $'            // A leaving agent answers no read any more.\n            self.fail_reads_of(conn);\n' "" \
  --lib -- hub::slots::tests::a_brief_read_out
probe "C5 Agent calls past the first batch wait for the next lookup" $S \
  "match self.reader(&session).filter(|_| more) {" "match self.reader(&session).filter(|_| false) {" \
  --lib -- reads:: hub::slots::tests::three_explicit
probe "C6 a candidate is looked up again only on its stop" $S \
  "            self.ask_calls(conn, &session, path, from);
        }
    }" "            let _ = (conn, from); self.match_candidates(&session);
        }
    }" --lib -- hub::slots::tests::three_explicit hub::slots::tests::a_stop_before

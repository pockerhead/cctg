#!/usr/bin/env bash
# Reverts each fix in turn and checks that its new test fails; restores the file.
set -u
cd /c/Users/user/dev/cctg
export CARGO_TARGET_DIR="$TEMP/cctg-t015-fix-target" CARGO_PROFILE_DEV_DEBUG=0
F=crates/cctg/src/hub/slots.rs
cp "$F" "$F.bak"
run() { cargo test -j 1 -p cctg --lib "$1" 2>&1 | grep -E "^test result|panicked" | head -3; }
echo "== M1: no index removal at end"
sed -i 's/                self.indexes.remove(session);/                let _ = session;/' "$F"
run the_agent_calls_of_an_ended_session_are_forgotten
cp "$F.bak" "$F"
echo "== M2: nested block only when running"
sed -i 's/.filter(|block| block.running || answer.is_some())/.filter(|block| block.running)/' "$F"
run a_nested_answer_after_its_parent_ended_still_shows
cp "$F.bak" "$F"
echo "== M3: no saved header"
sed -i 's/header: Some(entry.block.header.clone()).filter(|header| !header.is_empty()),/header: None,/' "$F"
run the_agent_calls_of_an_ended_session_are_forgotten
cp "$F.bak" "$F"
rm "$F.bak"

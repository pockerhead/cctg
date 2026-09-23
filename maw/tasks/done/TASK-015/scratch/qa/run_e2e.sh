#!/usr/bin/env bash
# QA TASK-015: run the QA end-to-end test (copied into crates/cctg/tests only
# for the run), then one mutation to prove it is not vacuous.
# Usage: run_e2e.sh [mutation]   mutation: none | ghost | target
set -u
cd C:/Users/user/dev/cctg
export CARGO_TARGET_DIR="$TEMP/cctg-t015-qa-target"
export CARGO_PROFILE_DEV_DEBUG=0
QA=maw/tasks/in_progress/TASK-015/scratch/qa
MUT=${1:-none}
cp $QA/qa_e2e.rs crates/cctg/tests/qa_e2e.rs
SLOTS=crates/cctg/src/hub/slots.rs
cp $SLOTS $QA/slots.rs.orig
case $MUT in
  ghost)
    # A candidate whose Agent call is not found is confirmed anyway.
    python - <<'PY'
import re,io
p='crates/cctg/src/hub/slots.rs'
s=io.open(p,encoding='utf-8',newline='').read()
old="""                None => {
                    if self
                        .candidates
                        .missed(&agent_id, now, self.options.recheck_after)"""
new="""                None if true => self.confirm(&agent_id, AgentCall::default()),
                None => {
                    if self
                        .candidates
                        .missed(&agent_id, now, self.options.recheck_after)"""
CR=chr(13); LF=chr(10)
s=s.replace(CR+LF,LF)
assert s.count(old)==1
io.open(p,'w',encoding='utf-8',newline='').write(s.replace(old,new).replace(LF,CR+LF))
PY
    ;;
  target)
    python - <<'PY'
import io
p='crates/cctg/src/hub/slots.rs'
s=io.open(p,encoding='utf-8',newline='').read()
old='meta.insert("target_agent".to_owned(), agent_id.to_owned());'
assert s.count(old)==1
io.open(p,'w',encoding='utf-8',newline='').write(s.replace(old,'let _ = agent_id;'))
PY
    ;;
esac
cargo test -j 1 -p cctg --test qa_e2e -- --nocapture > $QA/e2e_$MUT.out.txt 2>&1
echo "exit $?" >> $QA/e2e_$MUT.out.txt
cp $QA/slots.rs.orig $SLOTS
rm -f $QA/slots.rs.orig crates/cctg/tests/qa_e2e.rs
git status --short
tail -5 $QA/e2e_$MUT.out.txt

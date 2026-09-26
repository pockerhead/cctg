#!/bin/sh
# Fixer (TASK-060): runs each changed e2e test binary 10 times; prints pass counts.
set -u
export CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0
cd C:/Users/user/dev/cctg-060 || exit 1
out=maw/tasks/in_progress/TASK-060/scratch/runs/fixer_repeat
mkdir -p "$out"
for t in permission_hook_e2e question_hook_e2e status_e2e hook_cli statusline_cli statusline_agent_e2e reap_e2e; do
  pass=0
  for i in 1 2 3 4 5 6 7 8 9 10; do
    if cargo test -q -j 1 -p cctg --test "$t" > "$out/$t.last.txt" 2>&1; then
      pass=$((pass + 1))
    else
      cp "$out/$t.last.txt" "$out/$t.fail_$i.txt"
    fi
  done
  echo "$t $pass/10" | tee -a "$out/summary.txt"
done
echo DONE >> "$out/summary.txt"

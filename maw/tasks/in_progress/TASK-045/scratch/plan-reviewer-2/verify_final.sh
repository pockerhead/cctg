#!/usr/bin/env bash
# Run from anywhere in the repo after applying scratch/planner/task045.patch and then scratch/plan-reviewer-2/amendments.patch
# (git apply --ignore-whitespace, or by hand). Hashes are of LF bytes (CR
# stripped), so the core.autocrlf checkout matches.
cd "$(git rev-parse --show-toplevel)" || exit 1
H="maw/tasks/in_progress/TASK-045/scratch/plan-reviewer-2/hashes_final.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

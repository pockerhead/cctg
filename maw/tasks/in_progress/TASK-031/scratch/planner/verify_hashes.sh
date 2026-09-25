#!/usr/bin/env bash
# Run from anywhere in the repo after applying TASK-035's task035.patch and
# then scratch/planner/task031.patch (git apply --ignore-whitespace). Hashes
# are of LF bytes (CR stripped), so a core.autocrlf checkout matches.
cd "$(git rev-parse --show-toplevel)" || exit 1
H="maw/tasks/in_progress/TASK-031/scratch/planner/hashes.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

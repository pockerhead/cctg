#!/usr/bin/env bash
# Run after applying task012.patch. Hashes are of LF bytes (CR stripped).
cd "$(git rev-parse --show-toplevel)" || exit 1
H="maw/tasks/in_progress/TASK-012/scratch/planner/hashes.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

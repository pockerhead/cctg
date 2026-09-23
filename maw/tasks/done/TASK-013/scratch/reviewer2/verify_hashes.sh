#!/usr/bin/env bash
# Run from anywhere inside the repo after applying reviewer2/task013.patch.
# Hashes are of LF bytes (CR stripped: the Windows checkout may be CRLF).
cd "$(git rev-parse --show-toplevel)" || exit 1
H="maw/tasks/in_progress/TASK-013/scratch/reviewer2/hashes.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

#!/usr/bin/env bash
# Run from anywhere in the repo after applying scratch/reviewer2/task044.patch
# (git apply --ignore-whitespace on a core.autocrlf checkout). Hashes are of
# LF bytes (CR stripped), so a CRLF checkout matches.
cd "$(git rev-parse --show-toplevel)" || exit 1
H="maw/tasks/in_progress/TASK-044/scratch/reviewer2/hashes.txt"
fail=0
while read -r hash path; do
  path=${path%$'\r'}
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < <(tr -d '\r' < "$H")
exit $fail

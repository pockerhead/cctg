#!/usr/bin/env bash
# Usage (from the repo root): bash maw/tasks/in_progress/TASK-010/scratch/reviewer2/verify_hashes.sh
# Compares each touched file, CR stripped (core.autocrlf=true writes CRLF), with hashes.txt (LF bytes).
set -u
H="$(dirname "$0")/hashes.txt"
fail=0
while read -r sum path; do
  path="${path#\*}"
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$sum" ]; then echo "OK   $path"; else echo "DIFF $path"; fail=1; fi
done < "$H"
exit $fail

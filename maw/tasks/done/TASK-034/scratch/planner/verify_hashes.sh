#!/usr/bin/env bash
# Checks a tree against the planner's reference implementation of TASK-034.
# Run from the repo root: bash maw/tasks/in_progress/TASK-034/scratch/planner/verify_hashes.sh
# Hashes are of the file content with CR removed (autocrlf-proof).
# A mismatch is not an error by itself: the implementer may differ from the
# reference; it only says where.
set -u
here="$(dirname "$0")"
status=0
while read -r want file; do
  if [ ! -f "$file" ]; then
    echo "MISSING  $file"; status=1; continue
  fi
  got="$(tr -d '\r' < "$file" | sha256sum | cut -d' ' -f1)"
  if [ "$got" = "$want" ]; then echo "same     $file"; else echo "differs  $file"; status=1; fi
done < "$here/reference.sha256"
while read -r file; do
  if [ -e "$file" ]; then echo "PRESENT  $file (the reference deletes it)"; status=1; fi
done < "$here/reference.deleted"
exit $status

#!/usr/bin/env bash
# Run from the repo root after applying task011.patch. Hashes are of LF bytes.
cd "$(git rev-parse --show-toplevel)" || exit 1
H="$(dirname "$0")/hashes.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

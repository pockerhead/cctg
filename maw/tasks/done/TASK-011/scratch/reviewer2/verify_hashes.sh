#!/usr/bin/env bash
# Run from the root of the checkout that has task011_final.patch applied.
# Hashes are of LF bytes (CR stripped), so core.autocrlf does not matter.
H="$(cd "$(dirname "$0")" && pwd)/hashes.txt"
fail=0
while read -r hash path; do
  got=$(tr -d '\r' < "$path" | sha256sum | cut -d' ' -f1)
  if [ "$got" = "$hash" ]; then echo "OK $path"; else echo "MISMATCH $path"; fail=1; fi
done < "$H"
exit $fail

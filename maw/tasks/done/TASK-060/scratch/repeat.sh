#!/bin/sh
# usage: repeat.sh <n> <test-exe> [test filter args...]  -> prints pass/fail counts
n=$1; shift; exe=$1; shift
OUT=${OUT:-$(dirname "$0")/runs}; mkdir -p "$OUT"; pass=0; fail=0
for i in $(seq 1 "$n"); do
  if "$exe" "$@" > "$OUT/last.txt" 2>&1; then pass=$((pass+1)); else fail=$((fail+1)); cp "$OUT/last.txt" "$OUT/fail_$fail.txt"; grep -E "panicked|FAILED" "$OUT/last.txt" | head -5; fi
done
echo "pass=$pass fail=$fail"

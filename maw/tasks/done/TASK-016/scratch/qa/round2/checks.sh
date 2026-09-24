#!/bin/bash
# QA round 2: fmt, clippy, full workspace test with one target dir.
export CARGO_TARGET_DIR="$TEMP/cctg-qa2-016-target"
export CARGO_PROFILE_DEV_DEBUG=0
OUT=C:/Users/user/dev/cctg/maw/tasks/in_progress/TASK-016/scratch/qa/round2
cd C:/Users/user/dev/cctg
cargo fmt --all -- --check > $OUT/fmt.out.txt 2>&1; echo "fmt rc=$?" >> $OUT/fmt.out.txt
cargo clippy -j 1 --workspace --all-targets -- -D warnings > $OUT/clippy.out.txt 2>&1; echo "clippy rc=$?" >> $OUT/clippy.out.txt
cargo test -j 1 --workspace --no-fail-fast > $OUT/workspace_test.txt 2>&1; echo "test rc=$?" >> $OUT/workspace_test.txt

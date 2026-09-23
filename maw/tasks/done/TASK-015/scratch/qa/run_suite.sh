#!/usr/bin/env bash
# QA TASK-015: full workspace gates, one cargo at a time, one target dir under %TEMP%.
set -u
cd C:/Users/user/dev/cctg
export CARGO_TARGET_DIR="$TEMP/cctg-t015-qa-target"
export CARGO_PROFILE_DEV_DEBUG=0
OUT=maw/tasks/in_progress/TASK-015/scratch/qa
cargo fmt --all -- --check > $OUT/fmt.out.txt 2>&1; echo "fmt exit $?" >> $OUT/fmt.out.txt
cargo clippy -j 1 --workspace --all-targets -- -D warnings > $OUT/clippy.out.txt 2>&1; echo "clippy exit $?" >> $OUT/clippy.out.txt
cargo test -j 1 --workspace --no-fail-fast > $OUT/workspace.out.txt 2>&1; echo "test exit $?" >> $OUT/workspace.out.txt

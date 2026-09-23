#!/usr/bin/env bash
# TASK-015 implementer checks: one target dir under TEMP, -j 1, one cargo at a time.
cd "$(git rev-parse --show-toplevel)" || exit 1
export CARGO_TARGET_DIR="${TEMP:-/tmp}/cctg-t015-impl-target"
export CARGO_PROFILE_DEV_DEBUG=0
OUT=maw/tasks/in_progress/TASK-015/scratch/implementer
bash maw/tasks/in_progress/TASK-015/scratch/reviewer2/verify_hashes.sh > $OUT/hashes.out.txt 2>&1
cargo fmt --all --check > $OUT/fmt.out.txt 2>&1; echo "fmt exit=$?" >> $OUT/fmt.out.txt
cargo test -j 1 -p transcript --test subagent > $OUT/transcript_subagent.out.txt 2>&1; echo "exit=$?" >> $OUT/transcript_subagent.out.txt
cargo test -j 1 -p cctg --lib hub:: > $OUT/hub.out.txt 2>&1; echo "exit=$?" >> $OUT/hub.out.txt
cargo test -j 1 --workspace --no-fail-fast > $OUT/workspace.out.txt 2>&1; echo "exit=$?" >> $OUT/workspace.out.txt
cargo clippy -j 1 --workspace --all-targets -- -D warnings > $OUT/clippy.out.txt 2>&1; echo "exit=$?" >> $OUT/clippy.out.txt

#!/usr/bin/env bash
# Type-checks and clippy-lints the TASK-044 Unix code on Windows, without
# Linux/macOS targets installed: a mini crate named `cctg` holding the real
# term.rs, keys.rs, run.rs, the macOS part of proctree.rs and
# tests/run_pty_e2e.rs verbatim (update/wire stubbed, *.rs.txt here), built
# with std from rust-src (RUSTC_BOOTSTRAP=1 -Zbuild-std; stable 1.95).
# Usage: unixcheck.sh <workspace root with the patch applied> <mini dir>
# The mini dir must be outside the repo (TASK-018).
set -euo pipefail
WS=$1; M=$2; HERE=$(cd "$(dirname "$0")" && pwd)
S=$WS/crates/cctg
mkdir -p "$M/src" "$M/tests/common"
cp "$HERE/Cargo.toml.txt" "$M/Cargo.toml"
cp "$WS/Cargo.lock" "$M/"
for f in lib update wire; do cp "$HERE/$f.rs.txt" "$M/src/$f.rs"; done
for f in term keys run; do tr -d '\r' < "$S/src/$f.rs" > "$M/src/$f.rs"; done
tr -d '\r' < "$S/tests/run_pty_e2e.rs" > "$M/tests/run_pty_e2e.rs"
tr -d '\r' < "$S/tests/common/mod.rs" | sed 's/env!("CARGO_BIN_EXE_cctg")/"cctg"/' > "$M/tests/common/mod.rs"
P=$(tr -d '\r' < "$S/src/proctree.rs")
start=$(grep -n '^#\[cfg(target_os = "macos")\]$' <<<"$P" | awk -F: '$1>400{print $1; exit}')
end=$(grep -n '^/// `pid (comm) state ppid' <<<"$P" | cut -d: -f1)
{
  echo '#![allow(dead_code)]'
  echo 'pub const MAX_DEPTH: usize = 64;'
  echo '#[derive(Debug, Clone, PartialEq, Eq)]'
  grep -n '' <<<"$P" | sed -n '/^[0-9]*:pub struct Proc {/,/^[0-9]*:}/p' | cut -d: -f2-
  sed -n "${start},$((end - 1))p" <<<"$P"
} > "$M/src/proctree.rs"
cd "$M"
export RUSTC_BOOTSTRAP=1 CARGO_PROFILE_DEV_DEBUG=0
for t in x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-apple-darwin; do
  echo "== $t"
  cargo clippy -j 1 -Zbuild-std=std,panic_unwind --target "$t" --lib --test run_pty_e2e -- -D warnings 2>&1 \
    | grep -v '^\s*\(Compiling\|Checking\)' || true
done

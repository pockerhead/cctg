#!/usr/bin/env bash
# Builds task011_final.patch and hashes.txt from reviewer2/ws against repo HEAD,
# in a throwaway local clone (the real working tree is never touched).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
CLONE="${TEMP:-/tmp}/cctg-task011-rev2-clone"
FILES="crates/cctg/src/hub/commands.rs crates/cctg/src/hub/mod.rs crates/cctg/src/hub/registry.rs crates/cctg/src/hub/sessions.rs crates/cctg/src/hub/slots.rs crates/cctg/tests/slots_logs.rs"
rm -rf "$CLONE"
git clone -q "$REPO" "$CLONE"
git -C "$CLONE" checkout -q "$(git -C "$REPO" rev-parse HEAD)"
for f in $FILES; do cp "$HERE/ws/$f" "$CLONE/$f"; done
git -C "$CLONE" add -A -- crates
git -C "$CLONE" diff --cached --binary -- crates > "$HERE/task011_final.patch"
: > "$HERE/hashes.txt"
for f in $FILES; do
  printf '%s  %s\n' "$(tr -d '\r' < "$HERE/ws/$f" | sha256sum | cut -d' ' -f1)" "$f" >> "$HERE/hashes.txt"
done
# Re-apply from scratch in the clone: the patch alone must reproduce ws.
git -C "$CLONE" reset -q --hard
git -C "$CLONE" apply --check "$HERE/task011_final.patch"
git -C "$CLONE" apply "$HERE/task011_final.patch"
(cd "$CLONE" && bash "$HERE/verify_hashes.sh")
# And it applies to the real repo root as it is now.
git -C "$REPO" apply --check "$HERE/task011_final.patch" && echo "APPLY_CHECK_REPO_OK"
git -C "$CLONE" status --short

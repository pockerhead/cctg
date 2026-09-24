#!/usr/bin/env bash
# Where the inbox's .gitignore goes decides what git hides (TASK-032, reviewer2).
# Usage: bash gitignore_probe.sh <empty temp dir>
set -e
cd "$1"
for variant in planner reviewer2; do
  rm -rf "$variant"; mkdir "$variant"; cd "$variant"
  git init -q; mkdir -p .cctg/inbox
  echo notes > .cctg/notes.md; git add .cctg/notes.md
  git -c user.email=x@x -c user.name=x commit -qm base
  echo pic > .cctg/inbox/2026-09-25-shot.png
  if [ "$variant" = planner ]; then printf '*\n' > .cctg/.gitignore; else printf '*\n' > .cctg/inbox/.gitignore; fi
  echo later > .cctg/later.md   # something else the project keeps in .cctg
  echo "== $variant: git status --porcelain -uall"
  git status --porcelain -uall
  cd ..
done

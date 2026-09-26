#!/usr/bin/env bash
# For files verify_final.sh reports MISMATCH: rebuild the reference result on
# 7748a43 (both patches) and check that ours differs from it only by the
# TASK-053 delta (7748a43 -> HEAD~0 of main), ignoring line numbers.
cd "$(git rev-parse --show-toplevel)" || exit 1
T=maw/tasks/in_progress/TASK-045/scratch
BASE=${BASE:-7748a43}; MAIN=${MAIN:-4c642fb}
R=$(mktemp -d); git archive $BASE | tar -x -C $R
(cd $R && git init -q && git apply --ignore-whitespace "$OLDPWD/$T/planner/task045.patch" && git apply --ignore-whitespace "$OLDPWD/$T/plan-reviewer-2/amendments.patch") || exit 1
strip() { sed -E 's/^[0-9,]+[acd][0-9,]+$/@@/'; }
fail=0
for f in "$@"; do
  git show $BASE:$f | tr -d '\r' > $R/base; git show $MAIN:$f | tr -d '\r' > $R/main
  tr -d '\r' < $R/$f > $R/refr; tr -d '\r' < $f > $R/ours
  diff $R/base $R/main | strip > $R/d1; diff $R/refr $R/ours | strip > $R/d2
  if cmp -s $R/d1 $R/d2; then echo "SAME-AS-TASK-053-DELTA $f"; else echo "EXTRA-DRIFT $f"; fail=1; fi
done
rm -rf $R; exit $fail

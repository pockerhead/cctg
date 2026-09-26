#!/bin/sh
# TASK-060 verification: 20 runs of each flaky test under CPU load + a
# parallel `cargo build --release -j 1`; the CLI tests run as two concurrent
# copies (as two worktrees sharing the target dir do).
T=C:/Users/user/dev/cctg/target/debug/deps
D=$(dirname "$0")
R=$D/runs/verify; rm -rf "$R"; mkdir -p "$R"
python "$D/cpu_load.py" 20 1500 > /dev/null 2>&1 &
(cd C:/Users/user/dev/cctg-060 && CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 cargo build --release -j 1 --workspace > "$R/release_build.txt" 2>&1) &
for t in hook_cli-b1885ce6542a5431 statusline_cli-c504c5cd54efdde7 spool_e2e-20547c0076cad925 question_hook_e2e-b642e35242917a28; do
  OUT=$R/${t}_twin sh "$D/repeat.sh" 20 $T/$t.exe > "$R/${t}_twin.summary" 2>&1 &
  OUT=$R/$t sh "$D/repeat.sh" 20 $T/$t.exe > "$R/$t.summary" 2>&1
  wait $!
done
OUT=$R/subagents sh "$D/repeat.sh" 20 $T/cctg-59f6df32de6016dc.exe three_explicit_subagents_make_three_blocks_and_internal_agents_none > "$R/subagents.summary" 2>&1
OUT=$R/terminal sh "$D/repeat.sh" 20 $T/question_hook_e2e-b642e35242917a28.exe the_terminal_button_gives_no_decision_at_once > "$R/terminal.summary" 2>&1
echo ALLDONE > "$R/done"

# PCTX proposals

## 2026-09-26 (TASK-056 code-reviewer): shared target dir across worktrees runs foreign test binaries
With several worktrees building into `C:/Users/user/dev/cctg/target` at once, a test binary can be rebuilt by a sibling worktree between our build and our run. Observed: `transcript --test purity::every_source_file_is_scanned` failed with `read_dir` NotFound (its `CARGO_MANIFEST_DIR` pointed at another, deleted worktree), and passed after a lone rerun recompiled it. `CARGO_BIN_EXE_cctg` has the same hazard. Proposed lesson: a failure in a crate the branch did not touch, under a shared target, is rerun alone before it is attributed to the change.

# FIX_SUMMARY — TASK-020

## Fixed

1. **Major: message-first slash commands were hidden.** `slash_command` now accepts either `<command-name>` or `<command-message>` as the leading tag and locates `<command-name>` and `<command-args>` in the complete record. Both brief and full render the real message-first form as a one-line prompt.
2. **Minor: a missing `<command-args>` hid the command.** A wholly absent args tag is now treated as empty args, so the command name is still rendered.
3. **Minor: a literal `</command-args>` truncated args.** Args now end at the last closing tag, preserving earlier literal closing-tag text.
4. Added the required anonymized `/maw-tasks add a task` message-first record to both `crates/transcript/tests/fixtures/slash_command.jsonl` and `scratch/slash_command.jsonl`. Added coverage for message-first order, absent args, and an embedded closing args tag. The existing tests continue to verify that unrelated service prefixes remain hidden in brief.

## Skipped

- No review issue was skipped.
- The review's blanket suggestion to use `split_once` for tag extraction was not followed verbatim for the args closing tag: doing so would preserve issue 3. `rsplit_once` is used there instead.
- The unrelated pre-existing change in `metrics.md` was left untouched.

## Test results

- `cargo test -p transcript` — PASS: 74 tests passed, 0 failed (including the compile-fail doctest).
- `cargo build --workspace` — PASS: workspace built successfully.
- `cargo test --workspace` — PASS: 110 tests passed, 0 failed, 1 ignored.
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS: no warnings.
- `cargo fmt --all -- --check` — PASS: formatting is clean.
- `git diff --check` — PASS: no whitespace errors.

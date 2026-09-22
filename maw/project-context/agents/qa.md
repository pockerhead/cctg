## Tooling (qa)
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. All three must be clean.
- For `crates/transcript`: render a real jsonl from `~/.claude/projects/C--Users-user-dev-cctg/` with the brief and full renderers and inspect the output by eye; report anything that leaks `thinking`, user ids or tokens.
- For hub/agent: runtime checks need a live Telegram bot and are manual; state clearly which checks were not executed and why.

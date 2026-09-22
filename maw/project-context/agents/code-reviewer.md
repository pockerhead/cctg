## Tooling (code-reviewer)
- Verify claims by running `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`, not by reading the implementer notes.
- Check that no crate outside the agreed set was added (`tokio`, `serde`, `serde_json`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `clap`, `dotenvy`, `reqwest`; `frankenstein` only as sanctioned fallback; never `teloxide` or `rmcp`) and that nothing prints to stdout inside the channel MCP server.

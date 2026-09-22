## Tooling (fixer)
- Build: `cargo build --workspace`. Lint: `cargo clippy --workspace --all-targets -- -D warnings`. Format: `cargo fmt --all`.
- Tests: `cargo test --workspace`. New behaviour in `crates/transcript` requires a test against a fixture in `crates/transcript/tests/fixtures/`.
- Windows host (PowerShell / Git Bash). Paths in tests must be OS-agnostic: use `std::path`, never string concatenation with backslashes.
- Do not add crates outside the agreed set (`tokio`, `serde`, `serde_json`, `anyhow`, `thiserror`, `tracing`, `tracing-subscriber`, `clap` (derive), `dotenvy`, `reqwest` with rustls and `default-features = false`; `frankenstein` only as the sanctioned fallback for Bot API types; no `teloxide`, no `rmcp`; `unicode-segmentation` only in `crates/transcript`) without recording the reason in the task NOTES.

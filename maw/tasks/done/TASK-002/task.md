# TASK-002: Bootstrap Cargo workspace and cctg CLI skeleton

Type: chore
Mode: small-fix
Priority: high
Branch: chore/bootstrap-workspace
Domains: transcript, hub, channel, hooks

## Description
Создать workspace ровно из двух members: `crates/cctg` (единственный binary с подкомандами `hub`, `agent`, `hook <event>`) и `crates/transcript` (чистая библиотека). Подключить только базовые общие зависимости (`tokio`, `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber`, `clap`), настроить логирование в stderr и дополнить `.gitignore`. Никакой бизнес-логики, никакого отдельного `proto` крейта.

## Acceptance criteria
- [ ] `cargo build --workspace`, `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят на Windows
- [ ] workspace содержит ровно два package members и собирает ровно один executable `cctg`
- [ ] `cctg --help` показывает `hub`, `agent`, `hook`; никакой путь выполнения, кроме собственно вывода CLI, ничего не пишет в stdout
- [ ] `crates/transcript` не зависит от `tokio`, HTTP-клиента и filesystem-крейтов (проверяется `cargo tree -p transcript`)
- [ ] `.gitignore` покрывает `target/`, `.env`, `*.log`, `.cctg/`, `registry.json`; сборка не оставляет неигнорируемых файлов
- [ ] Existing tests pass

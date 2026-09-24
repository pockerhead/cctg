# TASK-031: install.sh for any machine

Type: feature
Mode: brainstorm
Priority: medium
Branch: feature/cctg-install
Domains: hub, hooks, channel

## Description
Сейчас рабочая связка настроена руками только на машине пользователя: `~/.cctg/bin/cctg.exe`, `~/.cctg/poc/{mcp.json,settings.json}` (MCP-сервер `cctg agent`, все хуки, в будущем statusline из TASK-029), `~/.cctg/device.env` с секретом hub, обёртки `~/.local/bin/claude-cctg(.cmd)`. Нужна одна команда `cctg install` для любой машины (Windows, Linux, macOS): кладёт бинарник в стабильное место, пишет конфиги с абсолютными путями этой машины, спрашивает адрес hub и секрет (или берёт из аргументов), ставит обёртку в PATH, проверяет связь с hub (`cctg doctor`), умеет `--uninstall` и обновление. Связано с удалённым hub в Docker (адрес hub не localhost) и с TASK-026 (deploy). Сначала brainstorm: что пишется куда на каждой ОС, как не трогать пользовательский `~/.claude/settings.json`, как обновлять уже установленное.

Решение пользователя (2026-09-24): установка как у maw, скриптом прямо из репозитория (`install.sh`, запуск через `curl -fsSL <raw url>/install.sh | sh` или из клона; для Windows без Git Bash — `install.ps1` с тем же поведением), а не подкомандой бинарника. Скрипт скачивает готовый бинарник из GitHub Releases под свою ОС/архитектуру (или собирает `cargo build --release`, если есть cargo и попросили), кладёт конфиги и обёртку, спрашивает адрес hub и секрет, умеет `--uninstall` и повторный запуск для обновления. `cctg doctor` остаётся в бинарнике как проверка.

## Acceptance criteria
- [ ] план установки для Windows/Linux/macOS с перечнем файлов и путей
- [ ] решение, как обёртка и хуки находят бинарник после обновления
- [ ] Existing tests pass

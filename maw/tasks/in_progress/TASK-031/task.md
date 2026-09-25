# TASK-031: install.sh for any machine

Type: feature
Mode: full
Priority: medium
Branch: feature/cctg-install
Domains: hub, hooks, channel

## Description
Сейчас рабочая связка настроена руками только на машине пользователя: `~/.cctg/bin/cctg.exe`, `~/.cctg/poc/{mcp.json,settings.json}` (MCP-сервер `cctg agent`, все хуки, в будущем statusline из TASK-029), `~/.cctg/device.env` с секретом hub, обёртки `~/.local/bin/claude-cctg(.cmd)`. Нужна одна команда `cctg install` для любой машины (Windows, Linux, macOS): кладёт бинарник в стабильное место, пишет конфиги с абсолютными путями этой машины, спрашивает адрес hub и секрет (или берёт из аргументов), ставит обёртку в PATH, проверяет связь с hub (`cctg doctor`), умеет `--uninstall` и обновление. Связано с удалённым hub в Docker (адрес hub не localhost) и с TASK-026 (deploy). Сначала brainstorm: что пишется куда на каждой ОС, как не трогать пользовательский `~/.claude/settings.json`, как обновлять уже установленное.

Решение пользователя (2026-09-24): установка как у maw, скриптом прямо из репозитория (`install.sh`, запуск через `curl -fsSL <raw url>/install.sh | sh` или из клона; для Windows без Git Bash — `install.ps1` с тем же поведением), а не подкомандой бинарника. Скрипт скачивает готовый бинарник из GitHub Releases под свою ОС/архитектуру (или собирает `cargo build --release`, если есть cargo и попросили), кладёт конфиги и обёртку, спрашивает адрес hub и секрет, умеет `--uninstall` и повторный запуск для обновления. `cctg doctor` остаётся в бинарнике как проверка.

Дополнение пользователя 2026-09-24: вместе с установщиком написать в корне репо `README.md`, максимально простой и понятный: что это, одна картинка/схема на пару строк, установка одной командой, как подключить бота и группу, как запустить сессию, что видно в Telegram, как обновлять. Без внутренностей архитектуры (они остаются в CLAUDE.md и docs/).

Решение пользователя 2026-09-25: делать сейчас, в режиме full (не brainstorm), чтобы быстро перевести hub на тестовый сервер и тестировать клиентов везде: Windows (эта машина), Linux (сервер пользователя) и macOS Apple Silicon. Установщик клиента ставит бинарник из GitHub Releases TASK-035 (windows x86_64, linux x86_64, macOS aarch64), пишет `device.env` с адресом hub, секретом и `CCTG_HUB_CERT_SHA256` (TLS pin из TASK-035), MCP-конфиг, settings с хуками и statusLine, обёртку `claude-cctg` (bash и cmd) через `cctg run`. Повторный запуск обновляет, `--uninstall` убирает только своё. Сервер: README и `docs/remote-hub.md` (TASK-035) дают путь docker compose; установщик для hub не нужен, если compose хватает.

## Acceptance criteria
- [ ] `README.md` в корне: короткий, по шагам, без внутренностей; человек без контекста ставит и запускает по нему
- [ ] `install.sh` (Linux, macOS) и `install.ps1` (Windows) ставят клиента с нуля и обновляют; e2e в CI на всех трёх ОС против локального поддельного релиза (без сети к GitHub в тесте), `--uninstall` проверен
- [ ] не трогают пользовательские `~/.claude/settings.json` и `~/.claude.json`; секрет не печатается и не попадает в логи
- [ ] решение, как обёртка и хуки находят бинарник после обновления
- [ ] Existing tests pass

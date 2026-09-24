# TASK-031: cctg install on any machine

Type: feature
Mode: brainstorm
Priority: medium
Branch: feature/cctg-install
Domains: hub, hooks, channel

## Description
Сейчас рабочая связка настроена руками только на машине пользователя: `~/.cctg/bin/cctg.exe`, `~/.cctg/poc/{mcp.json,settings.json}` (MCP-сервер `cctg agent`, все хуки, в будущем statusline из TASK-029), `~/.cctg/device.env` с секретом hub, обёртки `~/.local/bin/claude-cctg(.cmd)`. Нужна одна команда `cctg install` для любой машины (Windows, Linux, macOS): кладёт бинарник в стабильное место, пишет конфиги с абсолютными путями этой машины, спрашивает адрес hub и секрет (или берёт из аргументов), ставит обёртку в PATH, проверяет связь с hub (`cctg doctor`), умеет `--uninstall` и обновление. Связано с удалённым hub в Docker (адрес hub не localhost) и с TASK-026 (deploy). Сначала brainstorm: что пишется куда на каждой ОС, как не трогать пользовательский `~/.claude/settings.json`, как обновлять уже установленное.

## Acceptance criteria
- [ ] план установки для Windows/Linux/macOS с перечнем файлов и путей
- [ ] решение, как обёртка и хуки находят бинарник после обновления
- [ ] Existing tests pass

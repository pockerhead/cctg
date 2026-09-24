# TASK-037: package cctg as a Claude Code plugin

Type: feature
Mode: full
Priority: medium
Branch: feature/cctg-plugin
Domains: channel, hooks

## Description
Решение пользователя 2026-09-24. `--channels` принимает только плагины из allowlist (Anthropic `claude-plugins-official` или `allowedChannelPlugins` организации на Team/Enterprise; https://code.claude.com/docs/en/channels, раздел Research preview и Enterprise controls). Сейчас cctg запускается через ручные `mcp.json` + `settings.json` + `--dangerously-load-development-channels server:cctg`. Нужно оформить cctg как плагин Claude Code: манифест плагина с MCP-сервером (`cctg agent`) и хуками (все события, которые сейчас в `~/.cctg/poc/settings.json`, включая PermissionRequest и statusline из TASK-029, если плагины это поддерживают; проверить по докам plugins reference), свой маркетплейс в этом репозитории (или отдельном), так чтобы работало `/plugin marketplace add <repo>` + `/plugin install cctg@<marketplace>` и запуск `claude --dangerously-load-development-channels plugin:cctg@<marketplace>` (а для организаций с allowlist — `--channels plugin:cctg@<marketplace>`). Бинарник: как плагин находит `cctg`/скачивает его под ОС (связь с TASK-031 install.sh). Документация: как подать в `claude-plugins-official` (требования, процесс), что для этого не хватает. Проверить: сохраняется ли наша проверка вложенности, секрет hub, device.env.

## Acceptance criteria
- [ ] плагин ставится из маркетплейса и поднимает канал cctg и все хуки в новой сессии без ручных `--mcp-config/--settings`
- [ ] старый способ запуска (обёртка `claude-cctg`) продолжает работать или явно заменён с инструкцией миграции
- [ ] документ: запуск через плагин, путь к официальному allowlist и `allowedChannelPlugins` для организаций
- [ ] Existing tests pass

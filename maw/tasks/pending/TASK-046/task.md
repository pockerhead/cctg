# TASK-046: /join command and a local hub without Docker

Type: feature
Mode: full
Priority: medium
Branch: feature/join-and-local-hub
Domains: hub, hooks

## Description
Вынесено из TASK-031 (решение оркестратора 2026-09-25). 1) `/join` в теме General: только от allowlisted, бот отвечает готовой строкой установки клиента (адрес hub, pin сертификата, секрет через переменную окружения, URL установщика на тег); вместе с TASK-045 строка несёт одноразовый код вместо общего секрета. 2) `install.sh --hub --local`: hub на этой же машине без Docker (бинарник + `cctg supervise`, автозапуск по ОС: Windows — задача входа пользователя или ярлык автозагрузки без окна, Linux — systemd user unit, macOS — launchd agent), как текущая ручная схема на машине пользователя.

## Dependencies
- blocked by TASK-031 — installer base
- related TASK-045 — join codes replace the shared secret in the line

## Acceptance criteria
- [ ] `/join` отвечает только allowlisted, строка рабочая (e2e с фейковым Bot API)
- [ ] `--hub --local` ставит и запускает hub с автозапуском на Windows/Linux/macOS без видимых окон; `--uninstall` убирает
- [ ] Existing tests pass

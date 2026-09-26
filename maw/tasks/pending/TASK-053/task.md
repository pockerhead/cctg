# TASK-053: show compaction in Telegram

Type: feature
Mode: small-fix
Priority: high
Branch: feature/compact-status
Domains: hub, hooks

## Description
Запрос пользователя 2026-09-26: авто- и ручное сжатие контекста (`/compact`) должно быть видно в теме. Сейчас хук `PreCompact` отвергается (`hook.rs`), в настройках его нет.

- `cctg hook PreCompact`: принять событие (`trigger`: `manual`|`auto`, `custom_instructions` не передавать дальше — это текст пользователя), отправить в hub новым типом события (additive, без VERSION bump).
- hub: при `PreCompact` закреплённый статус темы (TASK-029) показывает «🗜 сжимаю контекст (авто|вручную)…» с временем начала/таймером, в ленте одна тихая строка. При `SessionStart source=compact` той же сессии — статус возвращается, в ленту строка «🗜 контекст сжат за N с» и, если есть цифры контекста из statusline (TASK-029), «X% → Y%». Если сжатие не закончилось (SessionEnd, таймаут 15 мин) — статус снимается без строки об успехе.
- `install.sh` и `docs/hook-settings.json`: группа `PreCompact` (как остальные хуки, короткий timeout). Ручные настройки этой машины оркестратор обновит сам.

## Acceptance criteria
- [ ] PreCompact (auto и manual) виден в статусе и одной строкой; завершение даёт строку с длительностью (и процентами, если известны) (тест с фейковым Bot API через настоящий `cctg hook`)
- [ ] custom_instructions не попадают ни в hub, ни в логи
- [ ] старый hub на новое событие не ломается; install.sh пишет группу PreCompact (install_e2e)
- [ ] Existing tests pass

# TASK-028: permission prompts the channel does not relay (PermissionRequest hook)

Type: feature
Mode: small-fix
Priority: high
Branch: feature/permission-hook
Domains: hub, hooks, channel

## Description
Живая проверка 2026-09-24: обычные запросы разрешений (default и auto mode) приходят через канал (`permission_request`, TASK-014), но диалог проверки безопасности auto mode («Dangerous rm operation on possibly-empty variable…», с автоотказом через ~1:44) в канал не пересылается; пользователь отменяет руками в терминале. Хук `PermissionRequest` при этом срабатывает (проба показала вызов на этот запрос; stdin: session_id, permission_mode, tool_name, tool_input, permission_suggestions, без tool_use_id; решение: stdout `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"|"deny","message"?}}}`, пустой вывод = без решения). Не проверено, блокирует ли ждущий хук показ диалога в терминале, поэтому дизайн должен быть безопасен в обоих случаях.
Дизайн: `cctg hook PermissionRequest` POST-ит запрос в hub на новый путь (`/v1/permission`, тот же секрет) и ждёт ответа. Hub ждёт до ~1.5 с канальный `permission_request` той же сессии с тем же `tool_name`; если он пришёл, отвечает хуку «без решения» сразу (кнопки идут через канал как сейчас). Если нет, показывает кнопки Allow/Deny той же машиной TASK-014 (id от hub), нажатие отдаёт решение ждущему хуку; таймаут 90 с (меньше автоотказа), SessionEnd, ушедший клиент или hub недоступен → хук ничего не печатает и выходит 0. Не блокировать Slots actor. Настройка хука в docs/hook-settings.json и docs/poc.md с `"timeout": 100`.

## Acceptance criteria
- [ ] запрос, который канал переслал, не даёт второго набора кнопок (хук выходит без решения за ≤ ~1.5 с)
- [ ] запрос без канального аналога показывает кнопки; Allow/Deny возвращаются из хука в формате Claude Code (тест через настоящий `cctg hook PermissionRequest` и `serve_hooks`)
- [ ] таймаут, SessionEnd, hub выключен → хук без решения, выход 0, кнопки закрыты
- [ ] без секретов и текста команды в логах
- [ ] Existing tests pass

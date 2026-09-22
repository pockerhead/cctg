# TASK-013: agent — Channel MCP server over stdio

Type: feature
Mode: full
Priority: high
Branch: feature/agent-channel-server
Domains: channel

## Description
Реализовать вручную написанную JSON-RPC поверхность канала: `initialize` (с `capabilities.experimental["claude/channel"]`, опционально `claude/channel/permission` и `tools`), `notifications/initialized`, `tools/list`, `tools/call` (`reply`), исходящие `notifications/claude/channel` и `.../permission`, входящий `.../permission_request`. Всё остальное — method-not-found. Persistent TCP к hub, сопоставление сессии через env `CLAUDE_CODE_SESSION_ID` и реестр хуков. Ключи meta только `[A-Za-z0-9_]+`: невалидные отбрасываются, а не переименовываются молча. Команда установки в user scope использует абсолютный путь к исполняемому файлу. stdout — исключительно JSON-RPC.

## Dependencies
- blocked by TASK-010 — hard prerequisite
- prefer after TASK-004 — soft ordering
- prefer after TASK-011 — soft ordering

## Acceptance criteria
- [ ] сценарий initialize → initialized → tools/list → tools/call выдаёт ровно по одному валидному JSON-объекту на строку stdout
- [ ] неизвестный метод возвращает `-32601`; битый внешний ввод не паникует и не убивает сервер без контролируемого ответа
- [ ] stdout никогда не содержит логов и текста паники, в том числе при недоступном hub; логи идут только в stderr или файл
- [ ] невалидный ключ meta отбрасывается, валидные ключи и значения сохраняются байт в байт
- [ ] разрыв и восстановление соединения с hub не завершают MCP-сервер и приводят к повторной регистрации той же сессии
- [ ] ручной запуск подтверждает баннер и доставку inbound в сессию; вложенный запуск не регистрируется как самостоятельный маршрутизируемый канал
- [ ] Existing tests pass

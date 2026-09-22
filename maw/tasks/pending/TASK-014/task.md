# TASK-014: permission relay end to end

Type: feature
Mode: full
Priority: high
Branch: feature/permission-relay
Domains: channel, hub

## Description
Транслировать `permission_request` в тему слота с кнопками Allow/Deny, приоритетом в планировщике и gate по allowlist. В `callback_data` только действие и пятибуквенный request id — гарантированно ≤64 байт. Побеждает первый ответ; поздний вердикт становится безвредным «уже решено». Итоговое сообщение hub ограничивает по 4096 символов независимо от того, что прислал Claude.

## Dependencies
- blocked by TASK-011 — hard prerequisite
- blocked by TASK-013 — hard prerequisite

## Acceptance criteria
- [ ] `callback_data` Allow/Deny укладывается в 64 байта и порождает ровно один соответствующий вердикт
- [ ] callback от отправителя вне allowlist не отправляет вердикт и не раскрывает деталей запроса
- [ ] второй или поздний callback идемпотентен: помечает запрос решённым и не шлёт второй вердикт
- [ ] длинный `input_preview` даёт сообщение ≤4096 символов, а permission-трафик обгоняет очередь транскрипта (проверяется на заполненной очереди)
- [ ] permission-запрос попадает в тему того слота, которому принадлежит сессия, даже если сессия сменилась в слоте после старта запроса
- [ ] живая проверка подтверждает: подтверждение из Telegram закрывает параллельный терминальный диалог
- [ ] Existing tests pass

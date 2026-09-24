# TASK-041: silent by default

Type: feature
Mode: small-fix
Priority: high
Branch: feature/silent-by-default
Domains: hub

## Description
Пользователь 2026-09-24: сообщения статуса (TASK-029) приходят со звуком уведомления. Правило: бот шлёт всё с `disable_notification: true`, кроме того, где от пользователя что-то ждут или что-то закончилось: финальный ответ хода (Stop, TASK-022/023), запрос разрешения (TASK-014/028), «субагент закончил» (TASK-033), предупреждение об обновлении (TASK-040, когда появится). Без звука: сообщение статуса, строки стрима и 💭, блоки субагентов, разделители сессий, уведомления буфера и прочие служебные. `editMessageText` и `pinChatMessage` уже без звука. Реализация: поле `notify: bool` в `Op::Send` (и `SendDocument`, если применимо) с явным значением в каждом месте создания; `send_message` ставит `disable_notification` при `notify == false`; склейка строк стрима (merge) сохраняет `notify` (склеиваются только молчащие строки).

## Acceptance criteria
- [ ] перечисленные «громкие» сообщения идут со звуком, все остальные с `disable_notification: true` (тест на фейковом Bot API проверяет поле в теле запроса)
- [ ] склейка не смешивает громкие и тихие сообщения
- [ ] Existing tests pass

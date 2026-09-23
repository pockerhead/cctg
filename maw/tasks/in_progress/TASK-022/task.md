# TASK-022: hub — deliver the turn's final answer from the Stop hook

Type: fix
Mode: small-fix
Priority: high
Branch: fix/stop-answer-delivery
Domains: hub, channel

## Description
Живой PoC 2026-09-23: сообщение из темы дошло в сессию, модель ответила в терминал и не вызвала `mcp__cctg__reply`, поэтому в Telegram ничего не пришло. Решение пользователя: ответ не должен зависеть от того, вызовет ли модель инструмент. Hub отправляет финальный ответ каждого хода top-level сессии в тему её слота прямо из хука `Stop` (`last_assistant_message` уже приходит в `HookEvent::Stop`), через планировщик и `transcript::split_for_telegram`, по тем же правилам маршрутизации, что `Reply` (только живая top-level текущая сессия слота; не для вложенных запусков и субагентов). Инструмент `reply` остаётся для дополнительных сообщений: инструкции канала меняются на «твой финальный ответ хода уйдёт в Telegram автоматически; `reply` используй только для дополнительных сообщений по ходу работы», чтобы ответ не приходил дважды. Вызовы инструментов по ходу хода — это TASK-016 (tail транскрипта на стороне агента), не эта задача. Заодно `docs/poc.md`: запуск из Git Bash требует `MSYS_NO_PATHCONV=1` (иначе `server:cctg` и пути искажаются, диалог каналов не появляется) и пример короткой обёртки `claude-cctg`.

## Dependencies
- blocked by TASK-021 — hard prerequisite

## Acceptance criteria
- [ ] `Stop` живой top-level текущей сессии слота с непустым `last_assistant_message` даёт в теме слота сообщение с этим текстом (разбивка и порядок по `split_for_telegram`, >4 кусков одним документом)
- [ ] `Stop` вложенного запуска, завершённой сессии или сессии без темы ничего не отправляет; пустой или отсутствующий `last_assistant_message` ничего не отправляет
- [ ] путь hook ingress и slots-актор не ждут Telegram (отправка через dispatch-задачу), лимит очереди отправок общий с `Reply`
- [ ] инструкции канала больше не требуют отвечать через `reply`; тест на текст `initialize` обновлён
- [ ] текст ответа, user id и секреты не пишутся в логи
- [ ] `docs/poc.md` дополнен запуском из Git Bash (`MSYS_NO_PATHCONV=1`) и примером обёртки
- [ ] Existing tests pass

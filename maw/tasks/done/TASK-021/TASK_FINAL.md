# TASK-021: hub — route topic messages to the session agent and replies back

Type: feature
Mode: full
Priority: high
Branch: feature/hub-message-routing
Domains: hub, channel

## Description
Недостающее звено PoC (найдено planner-ом TASK-013): hub принимает сообщение allowlisted-пользователя в теме слота и отправляет его агенту текущей живой сессии этого слота как `HubMsg::Inbound` (с meta: `chat_id`, `message_id`, `thread_id`, reply-to при наличии); `AgentMsg::Reply` от агента hub отправляет в тему слота этой сессии через планировщик, с разбивкой `transcript::split_for_telegram`. Сообщение в тему без живой сессии или без подключённого агента не теряется молча: пока буфера нет (TASK-017), пользователь получает короткое уведомление «сессия не на связи». Команды (`/brief`, `/full` и т.п.) и служебные сообщения тем не пересылаются как inbound. Никакого Telegram I/O inline на пути ingress (урок TASK-010).

## Dependencies
- blocked by TASK-011 — hard prerequisite
- blocked by TASK-013 — hard prerequisite

## Acceptance criteria
- [ ] текст allowlisted-пользователя в теме слота доходит до агента текущей сессии слота как один `Inbound` с корректной meta (ключи только `[A-Za-z0-9_]`); сообщения из General и чужих тем не уходят ни одному агенту
- [ ] `Reply` агента уходит в тему слота его сессии через планировщик; длинный ответ разбивается по правилам `split_for_telegram`, порядок кусков сохраняется
- [ ] сообщение в тему без живой сессии или без агента даёт одно короткое уведомление и не вызывает ошибок; сессия, привязанная после `/clear` по pid, получает inbound в своём слоте
- [ ] команды и служебные `forum_topic_*` не пересылаются как inbound; не-allowlisted отправитель не доходит до агента
- [ ] путь ingress и slots-актор не ждут Telegram; тест со «стоящим» Telegram показывает, что inbound к агенту продолжает доходить
- [ ] ни текст сообщений, ни user id, ни секреты не пишутся в логи
- [ ] Existing tests pass

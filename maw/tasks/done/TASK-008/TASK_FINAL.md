# TASK-008: hub — Telegram Bot API client and outbound scheduler

Type: feature
Mode: full
Priority: high
Branch: feature/hub-telegram-foundation
Domains: hub

## Description
Реализовать тонкий Bot API клиент на `reqwest`: envelope `{ok, result, description, parameters}`, узкие serde-структуры только с нужными полями и `#[serde(default)]`, методы `getMe`, `getChatMember`, `getUpdates` (long polling), `sendMessage`, `editMessageText`, `sendDocument`, `deleteMessage`, `answerCallbackQuery`, `createForumTopic`, `editForumTopic`, `getForumTopicIconStickers`. Конфиг из `.env`, chat id в форме `-100...`. На старте проверять право `can_manage_topics` через `getChatMember`. Gate по allowlist `from.id`. Все исходящие операции идут через один планировщик: token bucket 20 сообщений/минуту на группу, FIFO внутри темы, коалесинг повторных правок, отдельная полоса для мутаций тем, приоритет permission-трафика. Любой 429 уважает `retry_after`. Служебные сообщения тем распознаются по `is_topic_message`/`forum_topic_*` и игнорируются роутингом. Зафиксировать измерения (release binary size, clean build time, idle RSS) как основание для возможного будущего перехода на `frankenstein`.

## Dependencies
- blocked by TASK-002 — hard prerequisite

## Acceptance criteria
- [ ] отправитель вне allowlist не доходит до обработчиков; ни `from.id`, ни токен не попадают в логи ни на одном пути ошибки (включая URL в тексте ошибки reqwest)
- [ ] планировщик соблюдает 20 сообщений/минуту на группу в тесте с подменённым временем и сохраняет порядок внутри темы
- [ ] повторные правки одного сообщения коалесятся; смоделированный 429 повторяется не раньше `retry_after` и не порождает retry storm
- [ ] создание и правка тем идут отдельной полосой и не списываются из message-bucket; численный лимит мутаций нигде не захардкожен
- [ ] апдейты со служебными сообщениями тем (`forum_topic_created/edited/closed/reopened`) распознаются и не роутятся как пользовательский ввод
- [ ] десериализация апдейта с неизвестными полями и неизвестным типом апдейта не роняет поллинг
- [ ] отсутствие права `can_manage_topics` обнаруживается на старте с внятной ошибкой, а не при первом `createForumTopic`
- [ ] в заметках задачи записаны измеренные binary size / build time / idle RSS
- [ ] Existing tests pass

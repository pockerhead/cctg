# TASK-030: inbound quotes and forwarded messages

Type: feature
Mode: small-fix
Priority: high
Branch: feature/inbound-quotes
Domains: hub, channel

## Description
Живая проверка 2026-09-24: пользователь ответил в теме на сообщение (reply с цитатой) словом «Удаляй», а сессия получила только «Удаляй» и meta `reply_to_message_id`, без текста, на который отвечали, и связала ответ не с тем. Telegram присылает в update `reply_to_message` (с `text`/`caption`) и, при цитировании фрагмента, поле `quote` (`text`, `position`). Hub (`hub/updates.rs` `classify`, `hub/api.rs` `Message`) сейчас берёт только id. Нужно: при явном reply (не корень темы) класть в content перед текстом пользователя цитату: `quote.text`, если есть, иначе начало `reply_to_message.text`/`caption`, обрезанное (например до 500 символов с многоточием), в виде строк с префиксом `> `; meta `reply_to_message_id` остаётся. Пересланные сообщения (`forward_origin` / `forward_from*`) помечать: meta `forwarded="true"` и префикс «(переслано)» в content, чтобы сессия не принимала их за слова пользователя; пачку пересланных сообщений доставлять как обычно по одному. Текст цитаты и пересланного в логи не писать.

## Dependencies
- blocked by TASK-021 — hard prerequisite

## Acceptance criteria
- [ ] ответ на сообщение с выделенной цитатой даёт в content сессии цитату `quote.text` и текст пользователя; без выделения берётся начало исходного сообщения, длинное обрезается
- [ ] ответ на корень темы (неявный reply форума) цитату не добавляет
- [ ] пересланное сообщение помечено meta `forwarded` и префиксом; ключи meta только `[A-Za-z0-9_]`
- [ ] тесты на реальных формах update (фикстуры без user id), текст не попадает в логи
- [ ] Existing tests pass

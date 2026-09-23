# TASK-020: transcript — show user slash commands in brief

Type: fix
Mode: small-fix
Priority: high
Branch: fix/brief-slash-commands
Domains: transcript

## Description
Brief сейчас скрывает набранные пользователем slash-команды: записи `user` со строковым content вида `<command-name>/model</command-name><command-message>model</command-message><command-args>opus</command-args>` попадают под `SERVICE_PREFIXES` в `crates/transcript/src/render.rs`. На машине 43 такие не-meta записи. Решение пользователя (2026-09-23): это действия пользователя, а не шум. Brief и full показывают их одной строкой промпта `> /model opus` (без аргументов: `> /model`), берётся имя из `<command-name>` и текст из `<command-args>`. Такая запись считается границей промпта для правила «в работе…» как обычный промпт. Остальные служебные префиксы (`<local-command-stdout>`, `<local-command-caveat>`, task-notification, compact summary и прочие) остаются скрытыми в brief как сейчас.

## Dependencies
- blocked by TASK-006 — hard prerequisite

## Acceptance criteria
- [ ] запись со строкой `<command-name>/model</command-name>…<command-args>opus</command-args>` рендерится в brief и full как `> /model opus`; с пустыми args как `> /model`
- [ ] многострочные или содержащие `<`/`>` аргументы не ломают строку и не теряют имя команды
- [ ] команда без последующего ответа ассистента в хвосте получает маркер «в работе…» по тем же правилам, что обычный промпт
- [ ] `<local-command-stdout>` и остальные служебные префиксы по-прежнему скрыты в brief
- [ ] обезличенная фикстура с реальной формой записи команды; приватных данных в ней нет
- [ ] Existing tests pass

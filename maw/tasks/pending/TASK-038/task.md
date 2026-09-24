# TASK-038: answer AskUserQuestion from Telegram

Type: feature
Mode: full
Priority: medium
Branch: feature/ask-user-question
Domains: hub, hooks, channel

## Description
Пользователь (2026-09-24): когда Claude Code задаёт вопрос через `AskUserQuestion` (1-4 вопроса с вариантами, multiSelect, всегда есть «Другое»), вопрос должен приходить в тему с кнопками и ответ из Telegram должен доходить до claude. Документированный путь (https://code.claude.com/docs/en/hooks, разделы PreToolUse → AskUserQuestion и "Allow with updatedInput"): хук `PreToolUse` с matcher `AskUserQuestion` получает `tool_input.questions`; вернуть `hookSpecificOutput.permissionDecision: "allow"` вместе с `updatedInput` = исходные `questions` плюс `answers` (map: текст вопроса → label выбранного варианта; multiSelect — labels через запятую; «Другое» — свободный текст) отвечает программно. Нужно:
- `cctg hook PreToolUse` для `AskUserQuestion`: POST вопроса в hub (путь как `/v1/permission` из TASK-028 или отдельный), ожидание ответа с таймаутом; без ответа — пустой вывод, диалог в терминале идёт как обычно.
- hub показывает вопрос(ы) в теме: кнопки вариантов (callback ≤ 64 байт), для multiSelect — переключатели и «Готово», кнопка «Другое» принимает следующее текстовое сообщение (reply на вопрос) как ответ; после ответа сообщение редактируется в итог; allowlist, подпись автора (TASK-036, если есть).
- Проверить живьём/по докам: блокирует ли ждущий PreToolUse показ диалога в терминале (скорее да — тогда таймаут короче и кнопка «ответить в терминале» в Telegram, отпускающая хук сразу); не дублирует ли это permission relay (TASK-014/028: приходит ли на AskUserQuestion `permission_request` или `PermissionRequest` — если да, не показывать там Allow/Deny).
- ExitPlanMode (одобрение плана) — такой же путь, решить в плане, входит ли.

## Acceptance criteria
- [ ] вопрос AskUserQuestion приходит в тему с вариантами; выбор в Telegram возвращается в claude как ответ (тест через настоящий `cctg hook PreToolUse` и hub с фейковым Telegram)
- [ ] multiSelect и «Другое» (свободный текст) работают
- [ ] без ответа в срок терминальный диалог работает как раньше; нет второго набора кнопок Allow/Deny на тот же вопрос
- [ ] Existing tests pass

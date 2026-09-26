# PCTX proposals — TASK-038 (planner)

## 2026-09-26 — hooks domain: AskUserQuestion facts (after the probe confirms them)

Предлагаю добавить в `maw/project-context/domains/hooks.md` (и строкой в CLAUDE.md, раздел Hooks), когда оркестратор прогонит `scratch/planner/probe/probe_ask_question.py.txt` и результаты совпадут:

- `PreToolUse` с matcher `AskUserQuestion` получает `tool_input.questions` (`question`, `header`, `multiSelect`, `options[{label, description}]`) и `tool_use_id`. `permissionDecision: "allow"` без `updatedInput` на этот инструмент не отвечает; `allow` + `updatedInput` = исходный `tool_input` + `answers` (текст вопроса -> ответ) отвечает без диалога (docs hooks.md, PreToolUse decision control, якорь allow-with-updatedinput).
- Ответ multiSelect в терминале Claude Code склеивает labels через `", "` (запятая и пробел; транскрипт 2.1.252, `toolUseResult.answers`); свой текст («Other») кладётся как есть, и tool_result тогда начинается с `The user answered: ... Read the answers carefully`.
- Ждущий синхронный `PreToolUse` (результат пробы (a)) задерживает/не задерживает терминальный диалог; `statusMessage` хука виден в терминале, пока хук ждёт.
- `PermissionRequest` хук и канальный `permission_request` на `AskUserQuestion` приходят/не приходят (результат пробы (c)); ccgram (`permission-hook.ts`) отдельно выходит без решения на `AskUserQuestion`, то есть у них `PermissionRequest` на него срабатывал.

Почему: без этого каждая следующая задача про вопросы и план (ExitPlanMode) будет заново искать формат ответа и поведение диалога.

## 2026-09-26 — hub domain: questions share the permission lane

Предлагаю добавить в `domains/hub.md` одну строку о TASK-038 после мержа: вопросы `AskUserQuestion` живут в `hub/questions.rs` (чистая книга) + slots; сообщение на permission-полосе планировщика; ответ текстом (после «Другое» или reply на сообщение вопроса) перехватывается в `on_topic_message` до склейки TASK-048 и в сессию не уходит; hub ждёт 300 с, ingress 305 с, хук 310 с, settings `timeout` 330.

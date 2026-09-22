# TASK-006: transcript — brief/full rendering and Telegram sizing

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-renderers
Domains: transcript

## Description
Реализовать `render_brief` и `render_full` по нормативным правилам домена. Вывод MVP — plain text, чтобы не порождать невалидные Markdown-entity. Разбиение: сначала по turns и строкам, затем Unicode-safe жёсткий split; вернуть куски и явный признак «предпочесть отправку файлом» выше настраиваемого порога. Лимит считать в том же определении длины, которое использует Telegram-слой (символы после парсинга entities, для plain text — символы Unicode).

## Dependencies
- blocked by TASK-005 — hard prerequisite

## Acceptance criteria
- [ ] каждый текстовый кусок не превышает 4096 символов и никогда не режет UTF-8 последовательность или суррогатную пару эмодзи
- [ ] brief даёт ровно по одной строке на tool call без входов и результатов; full добавляет входы и усечённые результаты
- [ ] ни один рендерер не выдаёт `thinking` ни в каком режиме
- [ ] один блок в 50 KB и вход с эмодзи на границе куска дают детерминированные куски либо рекомендацию «файлом»
- [ ] рендер синтетического транскрипта на 5000 turns не растёт квадратично и укладывается в зафиксированный в тесте бюджет времени
- [ ] есть публичная функция рендера для набора turns (не только для всего файла), пригодная для инкрементального пуша в TASK-016
- [ ] парсер из TASK-005 сохраняет `message.stop_reason` в `Turn.stop_reason: Option<String>` (минимальная правка `crates/transcript`, тест на существующей фикстуре); остальная толерантность парсера не ослабевает
- [ ] brief показывает как ответ ассистента только текст из записи с `stop_reason == "end_turn"`; промежуточные тексты (`tool_use`) brief скрывает, full показывает
- [ ] незавершённый хвост (последний assistant-текст с `stop_reason: "tool_use"` без последующих записей, или ход без `end_turn`) brief рендерит пометкой «в работе…», а не выдаёт промежуточный текст за ответ; есть тест на такой хвост
- [ ] Existing tests pass

### Resolved questions
- 2026-09-22 (premise-challenge PREMISE SUSPECT, user approved amendment): the TASK-005 parser drops `message.stop_reason`, so a renderer over `&[Turn]` cannot tell an intermediate assistant text (`tool_use`, e.g. "let me check the file") from the final answer (`end_turn`). Orchestrator recount on this project's transcripts: text records 54 `end_turn` vs 46 `tool_use`. Criteria amended: carry `stop_reason` in `Turn`, brief shows only `end_turn` text, an unfinished tail renders as an in-progress marker.

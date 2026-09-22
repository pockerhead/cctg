# TASK-005: transcript — tolerant JSONL parser

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-parser
Domains: transcript

## Description
Реализовать `parse(&str) -> Vec<Turn>`: пропускать только записи `type: "user"` и `type: "assistant"`, читать лишь нужные поля и блоки с `#[serde(default)]`, молча пропускать неизвестные записи и блоки. Отдельной чистой функцией извлекать первый `ai-title` (в turn он не превращается). Никакого IO, никакого `unwrap()` на входных данных. Добавить обезличенные срезы реальных jsonl в `crates/transcript/tests/fixtures/`.

## Dependencies
- blocked by TASK-002 — hard prerequisite

## Acceptance criteria
- [ ] неизвестная запись, неизвестный блок и оборванная последняя строка не теряют ранее разобранные turns и не паникуют
- [ ] пустой вход и вход только из игнорируемых типов дают пустой вектор
- [ ] есть фикстуры: plain text, tool_use + tool_result, thinking + ai-title, sidechain-запись; ни одна не содержит приватного пути, токена или Telegram id
- [ ] `thinking` разбирается ровно настолько, чтобы его можно было гарантированно не отдать наружу, и ни один публичный API его не возвращает
- [ ] крейт не выполняет IO и не использует `unwrap()`/`expect()` на входных данных (проверяется clippy-lint или grep в тесте)
- [ ] `parse` на входе из произвольных байт (fuzz-подобный набор из десятка мусорных строк) не паникует
- [ ] `message.content` принимается в обеих формах для `user` и `assistant`: строкой (обычный промпт пользователя) и массивом блоков; строка превращается в один текстовый блок. Записи с `isMeta: true` разбираются и помечаются флагом в `Turn`, решение о показе остаётся рендереру (TASK-006)
- [ ] есть обезличенная фикстура со строковой user-записью, тест проверяет, что её текст не теряется
- [ ] Existing tests pass

### Resolved questions
- 2026-09-22 (premise-challenge PREMISE SUSPECT, user approved amendment): real transcripts contain `user` records whose `message.content` is a JSON string, not a block array (orchestrator recount on the project transcript dir: 42 non-meta + 16 `isMeta` string records vs 187 array records). Criteria amended: both content shapes are accepted, `isMeta` is carried as a flag, a string-content fixture is required.

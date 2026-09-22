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
- [ ] Existing tests pass

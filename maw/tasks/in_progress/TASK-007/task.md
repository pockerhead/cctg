# TASK-007: transcript — subagent data and collapsed rendering

Type: feature
Mode: full
Priority: medium
Branch: feature/transcript-subagents
Domains: transcript

## Description
Добавить чистые парсеры содержимого субагентского jsonl и опционального `.meta.json`, плюс модель свёрнутого блока `↳ <type> <id>`. Библиотека принимает строки и опциональные метаданные, файлы не открывает — выбор пути и чтение остаются в hub. Перехваченный отчёт субагента принимается отдельным опциональным входом и имеет приоритет над финальным текстом транскрипта.

## Dependencies
- blocked by TASK-005 — hard prerequisite
- prefer after TASK-006 — soft ordering

## Acceptance criteria
- [ ] sidechain-фикстура рендерится одним блоком `↳ <type> <id>` и не попадает в top-level turns родителя
- [ ] описание из `.meta.json` используется при наличии; отсутствующий или битый meta даёт безопасный fallback без ошибки
- [ ] переданный отчёт субагента становится телом блока вместо финального текста транскрипта
- [ ] тело блока субагента всегда в brief-форме, даже когда родитель рендерится в full
- [ ] API принимает `&str` и типизированные значения, filesystem-вызовов в крейте нет
- [ ] порядок fallback (отчёт → brief транскрипта → `last_assistant_message`) выражен в типах так, что hub не может его перепутать
- [ ] Existing tests pass

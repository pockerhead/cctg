# TASK-009: hub — local transcript commands

Type: feature
Mode: full
Priority: high
Branch: feature/hub-transcript-commands
Domains: hub, transcript

## Description
Первый вертикальный срез: команды `/brief [n]` и `/full [n]` читают локальный транскрипт по переданному пути, прогоняют через `transcript` и отвечают кусками через планировщик либо документом выше порога. Смещение `getUpdates` сохраняется атомарно, чтобы рестарт не обрабатывал команды повторно. Реестр слотов ещё не требуется.

## Dependencies
- blocked by TASK-006 — hard prerequisite
- blocked by TASK-008 — hard prerequisite

## Acceptance criteria
- [ ] `/brief` и `/full` на фикстурах совпадают с выводом библиотеки и сохраняют порядок кусков
- [ ] крупный вывод уходит документом; ошибка Telegram 400 по размеру один раз переключает доставку на документ, а не зацикливается
- [ ] сохранённое смещение исключает повторную обработку апдейта после смоделированного рестарта
- [ ] нечитаемый или отсутствующий путь даёт понятное пользователю сообщение, а поллинг продолжает работать
- [ ] ни логи, ни тесты, ни фикстуры не содержат токена, реальных user id и приватных путей
- [ ] путь транскрипта берётся из реального источника без реестра: команда `/brief [n] [session-id-prefix]` / `/full [n] [session-id-prefix]` ищет top-level `<session-id>.jsonl` под корнем проектов Claude Code (по умолчанию `~/.claude/projects`, в конфиге переопределяется, в тестах временный каталог); без префикса берётся самый свежий по mtime файл; префикс, совпавший с несколькими сессиями, даёт список кандидатов, а не угадывание. Резолвер спрятан за узким интерфейсом, который TASK-011 заменит на slot → current session
- [ ] `subagents/*.jsonl` никогда не выбираются как top-level сессия
- [ ] Existing tests pass

### Resolved questions
- 2026-09-23 (premise-challenge PREMISE SUSPECT, resolved by orchestrator under the user's full-autonomy authorization): `/brief [n]` carries no path and the hub has no registry yet, so the original slice could only test an injected fixture path. Amended: the command resolves the transcript from the Claude Code projects root (latest top-level session by mtime, or an explicit session-id prefix) behind a narrow resolver interface that TASK-011 replaces with slot -> current session. Alternative: wait for TASK-011. Would flip: the resolver forcing a registry-shaped API that TASK-011 cannot reuse.

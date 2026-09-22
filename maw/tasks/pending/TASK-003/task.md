# TASK-003: Spike — nested claude session identity

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-nested-session-detection
Domains: hooks

## Description
Временным локальным probe-хуком снять для двух сценариев (интерактивный старт и вложенный `claude -p` из Bash-тула) содержимое stdin хука и окружение: `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION`, плюс цепочку ppid. Сравнить env-id со stdin `session_id`, определить, работает ли основной признак вложенности и нужен ли fallback через `.cctg/<CLAUDE_PID>` и ppid. Заодно снять полный набор полей `SubagentStart`/`SubagentStop` и проверить, чем именно наполнено тело отчёта субагента. В репозитории нет `.claude/settings.json`, так что probe регистрируется во временном project-scope файле и удаляется после спайка. Production-код не писать.

## Dependencies
- unblocks TASK-012 — waits on this task
- unblocks TASK-015 — waits on this task

## Acceptance criteria
- [ ] в `scratch/` лежат redacted-захваты для top-level и nested старта; ни токенов, ни Telegram id, ни приватных абсолютных путей
- [ ] findings прямо отвечают: чей `session_id` видит вложенный hook в env, и какие из `CLAUDECODE`/`CLAUDE_CODE_CHILD_SESSION`/`CLAUDE_PID` сохраняются
- [ ] отдельно проверен и записан случай отсутствующей или перезаписанной env-переменной, и подтверждено/опровергнуто, что ppid-fallback его закрывает
- [ ] описан один контракт `detect_parent(hook_input, env, process_tree) -> Option<SessionId>` с явным порядком fallback
- [ ] зафиксирован фактический набор полей `SubagentStart`/`SubagentStop` и то, какое поле несёт итоговый отчёт субагента (а не предположение о нём)
- [ ] временный hook и временные settings удалены, что подтверждено read-only проверкой
- [ ] Existing tests pass

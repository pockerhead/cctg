# PCTX_PROPOSALS — TASK-003

## 2026-09-22 — домен hooks: признак вложенности опровергнут

Текущий текст maw/project-context/domains/hooks.md говорит: "Nesting detection: hook env
has CLAUDECODE=1 and CLAUDE_CODE_SESSION_ID differs from stdin session_id, so it is nested
and the parent is known. Fallback if the child claude overwrites env: ... ppid.
Which of the two actually works is an OPEN QUESTION: verify by running code."

Проверено кодом (8 сессий, scratch/FINDINGS.md, scratch/detect_parent_report.txt):

- Дочерний claude всегда перезаписывает CLAUDE_CODE_SESSION_ID и CLAUDE_PID. Условие
  "env != stdin" ложно во всех случаях, включая заведомо вложенные. Основной признак
  не работает; работает только ppid-путь.
- CLAUDE_CODE_CHILD_SESSION=1 стоит и у настоящего top-level. Формулировку про
  "children of a session inherit CLAUDECODE=1, CLAUDE_CODE_SESSION_ID, CLAUDE_PID,
  CLAUDE_CODE_CHILD_SESSION=1" стоит дополнить: ни одна из них не различает вложенность.
- У ppid-пути есть своя дыра: короткоживущий процесс-обёртка в цепочке обрывает обход,
  и вложенная сессия выглядит как top-level. Нужен третий исход, а не Option.

Предложение: заменить OPEN QUESTION на зафиксированный факт и на порядок fallback из
раздела "Контракт" в scratch/FINDINGS.md.

## 2026-09-22 — домен hooks: SubagentHandback не универсален

Текущий текст: "Since 2.1.271 a subagent using SubagentHandback delivers its report
through that tool; last_assistant_message is then only closing text."

Уточнение по факту: вызов SubagentHandback не гарантирован. Субагент Explore внутри
claude -p его не вызвал, и полный отчёт пришёл именно в SubagentStop.last_assistant_message.
Тот же тип субагента в интерактивной сессии его вызвал, и отчёт лежал в
PreToolUse.tool_input.message.

Предложение: зафиксировать правило выбора — если по agent_id был PreToolUse
SubagentHandback, берём tool_input.message, иначе SubagentStop.last_assistant_message.

## 2026-09-22 — побочно: .claude/settings.json подхватывается на лету

Уже запущенная интерактивная сессия начала исполнять только что добавленные хуки без
рестарта. Для установки cctg это плюс, но это же значит, что правка файла меняет
поведение живых сессий. Предложение: добавить одной строкой в раздел risk lessons
домена hooks.

## 2026-09-22 (fixer) — уточнения к первому предложению

- Признак "env != stdin" опровергнут и для настоящего интерактивного старта: claude.exe,
  поднятый в своей консоли с полностью очищенным окружением, всё равно проставляет свои
  CLAUDE_CODE_SESSION_ID и CLAUDE_PID (scratch/capture_E_interactive.jsonl).
- Дыра ppid-пути шире, чем было записано: все пять "top-level" запусков спайка на самом
  деле были вложенными, и top_level-вердикт им дал именно обрыв цепочки. Правило такое:
  цепочка цела — родитель находится всегда, цепочка оборвана — вложенность не видна.
- Отдельный риск реализации: протухший CLAUDE_PID (Windows переиспользует pid) заставляет
  обход вернуть собственную сессию как родителя. Нужна проверка parent != own session_id.

## 2026-09-22 (qa) — домен hooks: background_tasks не список живых субагентов

В SubagentStop самого субагента поле background_tasks всё ещё содержит его же запись со
status "running" (scratch/capture_unknown.jsonl, запись 11: agent ae1e99b76211e2536).
FINDINGS предлагает вести по background_tasks список живых субагентов темы — так делать
нельзя без поправки "агент из текущего SubagentStop считается завершённым".

Второе: фильтр "субагент тот, для кого раньше был SubagentStart" ломается, если хуки
поставлены посреди сессии. В capture_unknown настоящий субагент maw-implementer
(aa4e1bc6942527c3c) SubagentStart не имеет — он стартовал до установки хука. Более
устойчивый первичный признак в снятых данных: agent_type != "" (у внутренних агентов
Claude Code он пустой во всех 14 шумовых событиях).


> RESOLVED: all five proposals folded into domains/hooks.md and CLAUDE.md on 2026-09-22 (nesting contract, SubagentHandback rule, live settings reload, background_tasks caveat, agent_type filter).

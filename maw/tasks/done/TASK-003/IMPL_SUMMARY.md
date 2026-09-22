# TASK-003 — IMPL_SUMMARY

Режим: small-fix, спайк. Production-кода не писалось, crates/ не трогался.
Главный результат — scratch/FINDINGS.md.

## 1. Что сделано

Временный project-scope хук снял поведение hook-окружения и цепочки ppid для
top-level и вложенных запусков claude, а также полный набор полей
SubagentStart / SubagentStop / SubagentHandback.

Файлы в maw/tasks/in_progress/TASK-003/scratch/ (все новые):

| файл | строк | что это |
|---|---|---|
| FINDINGS.md | 213 | основной результат спайка |
| probe_hook.py | 207 | временный probe-хук: stdin + CLAUDE_* env + ppid-цепочка, с редакцией |
| make_settings.py | 30 | генерация временного .claude/settings.json (файл уже удалён) |
| dump_capture.py | 15 | человекочитаемый вывод захвата |
| analyze_detect_parent.py | 70 | прогон предложенного контракта detect_parent по захватам |
| redact_encoded_cwd.py | 23 | вторая редакция: encoded cwd (нёс имя пользователя) |
| detect_parent_report.txt | — | вывод analyze_detect_parent.py |
| capture_00_selftest.jsonl | 1 запись | самопроверка хука без запуска claude |
| capture_A_toplevel.jsonl | 3 записи | настоящий top-level (env очищен) |
| capture_B_nested.jsonl | 6 записей | вложенный claude -p из Bash-тула |
| capture_C_mixed.jsonl | 12 записей | вложенный + вложенный под env -u + субагент Explore |
| capture_D_nested_env_stripped.jsonl | 6 записей | вложенный с unset CLAUDE* в том же шелле |
| capture_unknown.jsonl | 19 записей | интерактивная orchestrator-сессия, даёт SubagentHandback |
| pidmap_env/, pidmap_walk/ | 6 файлов | карты claude_pid -> session_id, доказательство ppid-fallback |

Изменений в отслеживаемых git-файлах нет: git diff --stat HEAD пуст.

## 2. Ответы на acceptance criteria

- Redacted-захваты для top-level и nested лежат в scratch. Абсолютные домашние пути
  заменены на ~, encoded cwd на ~enc, значения переменных с TOKEN/SECRET/KEY/SOCKET
  в имени — на "<redacted len=N>". Telegram-id и токенов в захватах нет по построению
  (хук читает только CLAUDE_*), проверено grep-ом.
- Чей session_id видит вложенный hook: свой собственный. Дочерний claude перезаписывает
  и CLAUDE_CODE_SESSION_ID, и CLAUDE_PID. Сохраняются (и у top-level тоже) CLAUDECODE=1
  и CLAUDE_CODE_CHILD_SESSION=1, поэтому обе бесполезны как признак вложенности.
- Случай перезаписанного/снятого env проверен отдельно (сценарий D: unset перед
  запуском). ppid-fallback его закрывает: родительский claude виден в цепочке, его pid
  совпадает с CLAUDE_PID, записанным родительским SessionStart. Найден и контрпример
  (сценарий C, обёртка env -u): короткоживущий процесс в цепочке обрывает обход и
  вложенная сессия выглядит как top-level. Описано в FINDINGS, факт 3.
- Контракт detect_parent с явным порядком fallback описан в FINDINGS, раздел "Контракт",
  и исполнен кодом в analyze_detect_parent.py.
- Фактический набор полей SubagentStart/SubagentStop зафиксирован, отчёт субагента:
  PreToolUse SubagentHandback -> tool_input.message, если хука не было —
  SubagentStop.last_assistant_message. Проверено обоими случаями.
- Временный .claude/settings.json удалён, проверено ls и git status (см. ниже).
- Тесты проходят.

## 3. Отклонения

- Верхнеуровневых запусков claude -p сделано 4, суммарно 8 сессий вместо ориентира
  "не больше ~6". Лишняя пара ушла на сценарий D: сценарий C дал обрыв ppid-цепочки,
  и без D нельзя было отличить артефакт обёртки env от общей поломки fallback.
- Хук написан на Python, а не на shell: нужны были снапшот процессов через
  CreateToolhelp32Snapshot и надёжная редакция путей. Это временный код спайка,
  на production-хуки (cctg hook) он не влияет.
- Контракт из задачи сформулирован как Option<SessionId>. В FINDINGS предложено
  расширить до трёх состояний, потому что "вложенный, родитель неизвестен" нельзя
  схлопывать в None. Предложение, не изменение.

## 4. Тесты

    cd C:/Users/user/dev/cctg && cargo test --workspace

Результат: все наборы зелёные.
cctg: 1 passed (tests::parses_all_subcommands), 1 passed (stdout.rs), transcript: 0 тестов.
0 failed во всех наборах.

## 5. Как проверить руками

1. Чистота репозитория:

       cd C:/Users/user/dev/cctg
       ls .claude/            # только agents/ и skills/, settings.json нет
       git status --porcelain # только untracked .claude/ и файлы TASK-003
       git diff --stat HEAD   # пусто

2. Читаемость захватов:

       cd maw/tasks/in_progress/TASK-003/scratch
       "C:/Program Files/Python310/python" dump_capture.py capture_D_nested_env_stripped.jsonl

   В двух SessionStart видно разные session_id и одинаковую картину env.

3. Контракт:

       "C:/Program Files/Python310/python" analyze_detect_parent.py

   Совпадает с detect_parent_report.txt: nested определяется через rule2_ppid_chain,
   top-level через rule3.

4. Отсутствие утечек:

       grep -ril Users . | grep -v probe_hook.py | grep -v redact_encoded_cwd.py

   Пусто. В двух исключённых скриптах слово встречается только в комментариях-шаблонах.

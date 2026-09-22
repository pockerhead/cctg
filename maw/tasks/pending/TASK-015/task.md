# TASK-015: subagents and nested runs inside the parent slot

Type: feature
Mode: full
Priority: high
Branch: feature/subagents-nested-routing
Domains: hub, hooks, transcript

## Description
Связать явный родительский вызов `Agent` и его результат с `agent_id`; показывать только скоррелированные субагенты, а несопоставленные внутренние события не превращать в блоки-призраки. Тело блока на stop выбирается по порядку: перехваченный отчёт → brief по `subagents/agent-<id>.jsonl` → `last_assistant_message`. Вложенный `claude -p` прикрепляется к теме слота родителя как `⇣ nested <id>`, не создаёт ни слота, ни темы, ни маршрута канала. Ответ пользователя на блок субагента уходит в родительскую сессию с meta `target_agent=<agent_id>`.

## Dependencies
- blocked by TASK-007 — hard prerequisite
- blocked by TASK-011 — hard prerequisite
- blocked by TASK-012 — hard prerequisite
- blocked by TASK-013 — hard prerequisite
- prefer after TASK-014 — soft ordering

## Acceptance criteria
- [ ] три явных субагента в одной сессии дают одну тему и ровно три скоррелированных блока
- [ ] несопоставленные внутренние события `SubagentStop`, включая случай сессии, запущенной с `--agent`, не создают блоков-призраков
- [ ] фикстура с перехваченным отчётом показывает доставленный отчёт; весь порядок fallback покрыт тестами, включая отстающий файл транскрипта
- [ ] вложенный `claude -p` даёт ноль новых тем и ровно один блок `⇣ nested <id>` в теме родителя
- [ ] ответ на блок субагента приходит только в канал родителя и несёт валидный ключ meta `target_agent`
- [ ] рестарт hub не создаёт дублей тем и блоков; незавершённый блок помечается детерминированно
- [ ] Existing tests pass

# TASK-034: hub never reads session files (client/server split, part 1)

Type: refactor
Mode: full
Priority: high
Branch: feature/agent-serves-transcripts
Domains: hub, channel, transcript, hooks

## Description
Решение пользователя 2026-09-24: разделить cctg на клиентскую часть (агент + хуки на машине с claude) и сервер (hub с ботом и группой) и подготовить hub к работе на удалённом сервере. Первый шаг: hub не должен читать файлы машины сессии. Сейчас hub открывает jsonl по `transcript_path` для `/brief`, `/full`, `ai-title` в заголовке темы, блоков субагентов TASK-015 (корреляция по транскрипту родителя, тело из `subagents/agent-<id>.jsonl` и `.meta.json`) и, возможно, в других местах (найти все: grep по `std::fs`/`tokio::fs`/`spawn_blocking` в `hub/`). Всё это переводится на агент по существующему agent-линку, как стрим TASK-016 (`transcript_read`/`transcript_chunk` уже есть): hub запрашивает, агент читает у себя (тот же path gate: только файлы своей сессии под `<CLAUDE_CONFIG_DIR|~/.claude>/projects`, для субагентов её `subagents/`), отвечает ограниченными чанками. Для мёртвой сессии без агента `/brief` отвечает понятным сообщением (или использует последний закэшированный brief, если это дёшево; решить). Wire: новые сообщения за capability в Register, без VERSION bump; старый агент без capability даёт понятную деградацию. После задачи в `hub/` не остаётся чтения файлов, кроме собственного state (`registry.json`, offset, spool не его).

Решение оркестратора 2026-09-25 (PREMISE SUSPECT, PREMISE_CHALLENGE.md): живые top-level сессии без agent-линка по устройству (headless `claude -p` с `sdk-cli`, вложенные запуски с оборванной цепочкой процессов) деградируют так же, как мёртвая сессия без агента: `/brief`/`/full` дают понятный ответ, ai-title не обновляется, блок субагента строится из `SubagentStop` (`last_assistant_message`/handback) без чтения файлов. Отдельного линка только для отдачи файлов таким сессиям не делаем (редкий случай для claude-cctg, лишняя сложность).

## Acceptance criteria
- [ ] `/brief`, `/full`, заголовок из `ai-title`, блоки субагентов и вложенных запусков работают для сессий с агентом, когда hub не имеет доступа к файлам сессии; сессии без агента деградируют, как описано выше (тест: hub и агент с разными домашними/конфиг-папками, hub не видит путь)
- [ ] в `hub/` нет чтения файлов сессий (проверяемо grep-ом и тестом)
- [ ] агент отдаёт только файлы своей сессии, path gate покрыт тестами
- [ ] старый агент без capability: понятная деградация, без паники; VERSION не меняется
- [ ] Existing tests pass, soak (fake) зелёный

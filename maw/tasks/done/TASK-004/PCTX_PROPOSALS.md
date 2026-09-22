# PCTX proposals — TASK-004

## 2026-09-22 — domain `channel`: флаг не работает в headless `-p`

Что: доменный файл `channel.md` в инварианте про запуск говорит только
«Launch: `claude --dangerously-load-development-channels server:cctg`» и не
разделяет интерактивный и headless режим. Наблюдение показало, что в
`claude -p` подсистема каналов не инициализируется вообще: сервер спавнится как
обычный MCP, но `Channel notifications registered` не происходит, inbound
выбрасывается, `permission_request` не приходит. То же при `-p --resume`.

Почему важно: на этом стоит фаза 6 плана (headless resume мёртвых сессий,
TASK-019). Если инвариант не уточнить, следующий агент спроектирует двустороннюю
связь через канал в headless-сессии и это не заработает.

Доказательство: `maw/tasks/in_progress/TASK-004/scratch/FINDINGS.md` раздел 2,
`scratch/debug_M1c.log`, `scratch/debug_M2.log`.

## 2026-09-22 — domain `transcript` / CLAUDE.md: encoded cwd заменяет и `_`

Что: CLAUDE.md пишет, что encoded cwd — это путь с заменой `:`, `\`, `/` и
пробелов на `-`. Подчёркивание там не перечислено, но тоже заменяется: папка
`~\AppData\Local\Temp\cctg_probe_dir` дала каталог
`~enc-AppData-Local-Temp-cctg-probe-dir`.

Почему важно: парсер пути транскрипта по записанному правилу не найдёт файл для
любой папки с подчёркиванием в имени.

Доказательство: `scratch/FINDINGS.md` раздел 10, вывод
`ls ~/.claude/projects | grep -i Temp`.

## 2026-09-22 — domain `hooks`: `CLAUDE_PID` не гарантирована, `CLAUDE_CODE_ENTRYPOINT` полезна

Что: в интерактивных прогонах с очищенным окружением дочерний MCP-сервер видел
`CLAUDE_PID: null` — Claude Code сам её не выставляет. Зато
`CLAUDE_CODE_ENTRYPOINT` надёжно разделяет `cli` (интерактивная сессия) и
`sdk-cli` (`claude -p`, в том числе вложенный).

Почему важно: запасной вариант связки агента и хука через `CLAUDE_PID`/ppid
опирается на переменную, которой может не быть; а детект вложенности получает
дешёвый дополнительный признак.

Доказательство: `scratch/probe_log_N1.jsonl` (start-запись, `CLAUDE_PID: null`),
`scratch/probe_log_N4.jsonl` (две start-записи, `entry cli` и `entry sdk-cli`).

## 2026-09-22 — hub: транскрипта может не быть вовсе

Что: при унаследованной `CLAUDE_CODE_CHILD_SESSION=1` Claude Code выключает
сохранение транскрипта («Transcript saving is off»). Файла `.jsonl` не появляется
совсем.

Почему важно: `/brief` и `/full` должны корректно вести себя, когда файла нет,
а не считать это ошибкой чтения.

Доказательство: `scratch/screen_I4_banner.txt`, `scratch/FINDINGS.md` раздел 10.

## 2026-09-22 (fixer) — поправка к предложению про `CLAUDE_CODE_ENTRYPOINT` и `CLAUDE_PID`

Что: `CLAUDE_CODE_ENTRYPOINT=sdk-cli` означает «это `claude -p`», а не «это
вложенный запуск». Headless resume, который hub сам запустит (TASK-019), тоже
`sdk-cli`. Детект вложенности на нём строить нельзя, только в паре с контрактом
TASK-003 (дерево процессов + реестр pid). `CLAUDE_PID` в env MCP-сервера не
выставляется его claude, а наследуется: у вложенного агента это pid
родительского claude (N4: 17440), у чисто запущенной сессии `null`.

Почему важно: предложение выше формулирует entrypoint как «дешёвый
дополнительный признак» вложенности; в таком виде оно ведёт к ошибке для
hub-запущенных headless-сессий.

Доказательство: `scratch/probe_log_N4.jsonl` (start-записи: ppid 17440 / 2200,
`CLAUDE_PID` null / "17440"), `scratch/probe_log_M1*.jsonl` (`CLAUDE_PID` 15320 —
claude implementer'а, не их собственный).

## 2026-09-22 (QA) — domain `channel`: user-scope регистрация утекает во все живые сессии машины

Что: пока probe был зарегистрирован `--scope user`, его поднял и чужой,
не связанный со спайком claude пользователя в другом проекте (запись в
`scratch/probe_log_default.jsonl`, `entry=cli`, без флага). `claude mcp remove`
этот уже запущенный экземпляр не останавливает: процесс жил после уборки,
пока жила та сессия.

Почему важно: (1) для TASK-011 это подтверждение, что `cctg agent` будет
подниматься в каждой сессии устройства, включая сессии без флага, и hub обязан
показывать их как «нет канала»; (2) для спайков и тестов с user-scope сервером
уборка должна включать поиск и остановку уже запущенных экземпляров
(`Win32_Process` по командной строке), а не только `mcp remove`.

> RESOLVED: all proposals folded on 2026-09-22 into domains/channel.md (headless has no channel, user-scope leak into all sessions, entrypoint is headless not nested, CLAUDE_PID inherited, fresh-session caveat, cleanup risk lesson), domains/transcript.md (`_` in encoded cwd, missing transcript file) and CLAUDE.md.

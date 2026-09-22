# TASK-004 — QA_REPORT

Спайк, small-fix. Проверялось, подкреплены ли выводы `scratch/FINDINGS.md` сырыми
артефактами и годятся ли они для TASK-011 и TASK-013. Сессии claude не
запускались, MCP-серверы не регистрировались, код не менялся.

## 1. Environment

Прямой прогон в рабочем дереве `C:/Users/user/dev/cctg` (ветка
`chore/spike-channel-lifecycle`), docker и dev-сервера нет, runtime-проверок с
Telegram нет (hub ещё не написан). Всё ниже только чтение, кроме записи этого
отчёта, одной строки в `log.jsonl` и одного предложения в `PCTX_PROPOSALS.md`.

Команды:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff main --stat -- crates/ Cargo.toml Cargo.lock CLAUDE.md .claude maw/project-context
claude mcp list
grep -c probe ~/.claude.json
# сырые probe-логи: разбор start-записей (pid, ppid, env) и пар inbound -> permission_request -> tools/call
# сырой транскрипт implementer'а transcripts/6-implementer.jsonl: строки "cmd:" / "spawned pid"
# PowerShell: Get-CimInstance Win32_Process | ? CommandLine -match probe_channel_server
```

Сервисов не поднималось, останавливать нечего.

## 2. Test results

- `cargo fmt --check`: чисто. `cargo clippy -D warnings`: чисто.
- `cargo test --workspace`: `parses_all_subcommands` ok, `subcommands_do_not_write_to_stdout` ok,
  transcript lib и doc-tests 0/0. Падений нет.
- `git diff main --stat` по `crates/`, `Cargo.*`, `CLAUDE.md`, `.claude`,
  `maw/project-context`: пусто. Все 90 изменённых файлов лежат в `maw/tasks/in_progress/TASK-004/`.
- Скрипты автора (`selftest_probe.py` и т.д.) не перезапускались как проверка.

Собственные проверки (скрипты не сохранялись, это однострочники по сырым файлам):

| проверка | результат |
|---|---|
| контрпример: argv N2/N3 в сыром транскрипте vs ppid probe | `transcripts/6-implementer.jsonl:221` `--continue` spawned pid 7732 = ppid `probe_log_N2`; `:233` `--resume 684d8e75-…` pid 25092 = ppid `probe_log_N3`. Совпадает, контрпример не подтвердился |
| N2/N3: каждый inbound даёт permission_request и tools/call через 2 с | да, 2/2 и 2/2; session id 684d8e75 во всех start-записях N1..N5 |
| nonce N2/N3 на экране | `N2IIII` в `screen_N2_t70/final/nested`, `N3JJJJ` в `screen_N3_t60/t110/final` |
| N5 без флага | `debug_N5.log:239` connected, `:264` `not in --channels list`; 2 outbound в probe-логе, 0 входящих после `tools/list`; `N5LLLL` на экранах 0 |
| headless `-p` | в `debug_M1c.log`/`debug_M2.log` нет ни одной строки `Channel notifications` ни для какого сервера; M1/M1b/M1c/M2: perm=0, tools/call=0 |
| N4 вложенный | 2 start-записи: `cli` sid 684d8e75 `CLAUDE_PID=null`, `sdk-cli` sid f26f0436 `CLAUDE_PID=17440` (= ppid родительского probe) |
| `CLAUDE_CODE_CHILD_SESSION` отключает транскрипт | `screen_I4_banner.txt:11` дословно |
| N1 новая папка без диалогов | `screen_N1_startup.txt`: сразу баннер channels, диалогов нет; сырой `folder already known to claude: False` есть в транскрипте (4 вхождения) |
| редакция `scratch/` | 0 файлов с `Users\user`, `Users/user`, `C--Users-user`, `/c/Users/user`; e-mail 0; форма bot-token 0; `-100\d{10}` 0; `sk-ant`/`Bearer` 0 |
| `claude mcp list` | probe нет |
| `~/.claude.json` | `probe` встречается 1 раз: ключ `projects["~/AppData/Local/Temp/cctg_probe_dir"]`; `mcpServers` = web-reader, web-search-prime, zai-mcp-server, zread; probe ни в одном project `mcpServers` нет; временной папки нет |
| живые probe-процессы | **есть один**: python pid 14744, родитель claude pid 31992, см. баг 1 |
| log.jsonl | все ts на `Z`; dead_end implementer'а подтверждены (`debug_M1c/M2`, `transcripts/6-implementer.jsonl:125` `ACTIVATE_FAILED`) |

## 3. Acceptance criteria

| критерий | проверка | результат |
|---|---|---|
| таблица 4 режима × (баннер / `/mcp` / спавн / inbound / permission_request) | FINDINGS §1 против probe-логов, debug-логов, экранов | PASS. Все заполненные клетки сходятся с сырыми данными. Три клетки `/mcp` и одна permission без флага честно помечены UNVERIFIED |
| `--resume` и `--continue` наблюдались, а не выведены | argv из сырого транскрипта + pid/ppid + probe-логи + экраны | PASS |
| user-scope без per-project consent | `screen_N1_startup.txt`, pre-launch проверка в транскрипте | PASS, с оговоркой: причина отсутствия trust-диалога в новой папке не выяснена, и FINDINGS это прямо пишет |
| вложенный `claude -p` не становится самостоятельной маршрутизируемой регистрацией | `probe_log_N4.jsonl`, M1* | PASS в переформулировке фиксера: доказано, что канала у вложенного нет, а риск лишней регистрации в hub записан как риск для TASK-011, а не как «безопасно» |
| точная MVP-команда, alias, решение «без обёртки» | FINDINGS §9 | PASS (bash alias и PowerShell-функция синтаксически верны) |
| поведение без флага (тихий drop) | `debug_N5.log`, `probe_log_N5.jsonl`, экраны N5 | PASS для inbound. Permission без флага UNVERIFIED, помечено честно |
| probe-процесс и временная конфигурация удалены, секретов нет | `claude mcp list`, grep `~/.claude.json`, поиск процессов, grep по scratch | **FAIL**: жив процесс probe pid 14744 (баг 1, нигде не раскрыт); остался ключ `projects[…cctg_probe_dir]` (баг 2, раскрыт). Секретов нет |
| existing tests pass | fmt, clippy, test | PASS |

## 4. Bugs found

### 1. Major — probe-процесс жив в чужой сессии пользователя, в отчётах об этом ни слова

Воспроизведение:
```powershell
Get-CimInstance Win32_Process | ? { $_.CommandLine -match 'probe_channel_server' } | select ProcessId,ParentProcessId
# ProcessId 14744, ParentProcessId 31992 (claude, стартовал 20:59:35 по местному)
```
`scratch/probe_log_default.jsonl`, вторая start-запись 17:59:37Z: `entry=cli`,
`CLAUDE_PROJECT_DIR=~\dev\gamedev\VOIDRUN`, чужой session id 3bdf088c-…, записи
`stdin_eof` нет. Пока probe был зарегистрирован user-scope, пользователь
открыл claude в другом проекте, тот поднял probe, получил от него два inbound
(`NONONCE-T0`, `-T6`, без флага, скорее всего выброшены) и держит его до сих пор,
вместе с инструментом `mcp__probe__reply`. `claude mcp remove` уже запущенный
экземпляр не гасит.

Ожидалось: по AC «probe-процесс удалён» и раздел «Уборка» FINDINGS. Фактически
процесс жив, а FIX_SUMMARY пишет «прибирать нечего».

Исправление, действие пользователя: закрыть или перезапустить ту сессию claude,
либо `Stop-Process -Id 14744`. QA процесс не убивал: сессия чужая (решение в
`log.jsonl`). Когда процесс завершится, он допишет `stdin_eof` в
`scratch/probe_log_default.jsonl`, это безвредно.

Попутно это полезный факт для TASK-011, его нет в FINDINGS: user-scope сервер
поднимается в каждой сессии устройства, включая сессии без флага в других
проектах. Записано в `PCTX_PROPOSALS.md`.

### 2. Minor — в `~/.claude.json` остался ключ `projects[…cctg_probe_dir]`

Раскрыто в FINDINGS и FIX_SUMMARY. Ключ завёл сам Claude Code при запуске N1,
`mcpServers` чист, вреда нет. Скрипт `cleanup_probe_project_key.py` удаляет ровно
один ключ и проверяет round-trip. Одна оговорка к скрипту: это read-modify-write
файла, который живые сессии claude тоже пишут. Запускать, когда других сессий
нет, иначе можно затереть их свежую запись.

### 3. Minor — вывод I3 «непромптнутая сессия не начинает ход» опирается на один прогон без снимка экрана

FINDINGS §4 и decision в `log.jsonl` используют его как аргумент. Из сырых данных
видно только: три inbound приняты (`debug_I3.log:299,329,347`), `tools/call` 0.
Отсутствие транскрипта в I3 ничего не доказывает: у ранних прогонов сохранение
транскрипта было выключено унаследованным `CLAUDE_CODE_CHILD_SESSION`
(`screen_I4_banner.txt:11`). Экрана I3 нет, поэтому нельзя исключить, что ввод
блокировал какой-то экран. Практический вывод для hub («буферить на своей
стороне») от этого не меняется, но как факт это стоило пометить наблюдением на
n=1.

### 4. Nit — домашний путь в `PCTX_PROPOSALS.md:23-24`

`scratch/` вычищен полностью, а в предложении остался пример
`C:\Users\user\AppData\…` и `C--Users-user-…`. Имя профиля генерическое и такой
же пример уже есть в `CLAUDE.md`, так что это не утечка секрета. История
`b2dbc94` содержит неотредактированные захваты, фиксер это раскрыл.

## 5. Ответ на вопрос оркестратора

UNVERIFIED помечены честно. Три клетки `/mcp` (`--resume`, `--continue`, без
флага) и permission без флага нигде не выдаются за наблюдение, фраза «`/mcp`
одинаков с флагом и без» из FINDINGS убрана. Для TASK-011 и TASK-013 эти клетки не
несущие: `/mcp` канал не показывает даже когда он поднят, значит hub всё равно
определяет состояние по агенту; permission без канала hub и так не должен
ожидать.

Что TASK-011 и TASK-013 могут брать как проверенное:
- канал поднимается при fresh, `--resume`, `--continue` с флагом, session id при
  resume/continue не меняется, тема слота переживает это без логики;
- в `-p` (включая `-p --resume`) канала нет вообще, это ломает любую идею
  двусторонней связи через канал в headless (TASK-019);
- без флага inbound выбрасывается молча, без ack и ошибки;
- `input_preview` в `permission_request` приходит строкой;
- вложенный `-p` поднимает второй агент со своим session id, `sdk-cli` и
  унаследованным `CLAUDE_PID` родителя; отличать надо по контракту TASK-003,
  `ENTRYPOINT` не годится (им же будет помечен headless resume от hub);
- команда запуска и alias.

## 6. Verdict

**NEEDS_FIXES.** Содержание спайка поддержано сырыми данными и готово для
TASK-011/TASK-013, проверенный контрпример не подтвердился. Но AC «probe-процесс
удалён» не выполнен, и это не было раскрыто. Фиксы маленькие и не требуют
нового прогона: пользователь гасит pid 14744 (или закрывает ту сессию) и
запускает `cleanup_probe_project_key.py`, когда нет других сессий claude; в
«Уборку» FINDINGS дописать про утечку probe в чужую сессию и про оговорку I3.
После этого можно SHIP без повторного QA по существу.

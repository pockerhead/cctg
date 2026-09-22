# TASK-004 — development channel lifecycle and launch ergonomics

Всё ниже — наблюдения на живой машине, а не выжимка из документации.

Стенд: Windows 11 Pro 26200, Claude Code **2.1.280** (в CLAUDE.md записано 2.1.278 —
версия за время работы обновилась; поведение канала проверялось на 2.1.280),
Python 3.10.6, Git Bash. Probe — Python stdio JSON-RPC сервер
`scratch/probe_channel_server.py`, зарегистрирован user-scope:

```
claude mcp add --scope user probe -- python <abs path>/probe_channel_server.py
```

Probe объявляет `capabilities.experimental["claude/channel"] = {}` и
`["claude/channel/permission"] = {}`, отдаёт один инструмент `reply`, пишет весь
трафик в `scratch/probe_log_<scenario>.jsonl`. Stdout остаётся чистым JSON-RPC —
проверено `scratch/selftest_probe.py`, 8/8 PASS.

Главный источник доказательств спавна и доставки — `--debug-file`. В нём есть
ровно те строки, которые нужны:

- `MCP server "probe": Channel notifications registered`
- `MCP server "probe": notifications/claude/channel: PROBE-INBOUND-...`
- `MCP server "probe": Channel notifications skipped: server probe not in --channels list for this session`
- `MCP server "<other>": Channel notifications skipped: server did not declare claude/channel capability`

---

## 1. Таблица: 4 режима запуска

| режим | баннер | `/mcp` | спавн probe | inbound доставлен | permission_request |
|---|---|---|---|---|---|
| fresh + флаг, интерактив (I4, I5, N1) | да, отдельная плашка | probe connected, `Capabilities: tools`, канал не показан (I4) | да | да, 3 из 3 (I5) | да, MCP-tool 3 из 3 (I5); Bash в fresh не провоцировался |
| `--resume <id>` + флаг, интерактив (N3) | да, тот же баннер | **UNVERIFIED**: `/mcp` в этом режиме не снят | да | да, 2 из 2 | да (MCP-tool) |
| `--continue` + флаг, интерактив (N2) | да, тот же баннер | **UNVERIFIED**: `/mcp` в этом режиме не снят | да | да, 2 из 2 | да (Bash, request_id=tcmbm, и MCP-tool) |
| без флага, интерактив (N5) | нет плашки | **UNVERIFIED**: `/mcp` не снят; connected видно только в `debug_N5.log` | да | нет, тихо выброшен | 0 получено, но **UNVERIFIED**: действие, требующее разрешения, в N5 не запускалось |
| справочно: headless `-p` + флаг | н/д | в `system/init` probe connected | да | нет | нет |
| справочно: headless `-p --resume` + флаг | н/д | то же | да | нет | нет |

Команды прогонов. Точный argv каждого интерактивного прогона не только в этой
прозе: stdout лаунчера (`cmd: ...`, `spawned pid N`) и сами tool_use-команды
сохранены в транскрипте implementer'а, выжимка в `evidence_from_transcript.txt`
раздел 1. Pid, который напечатал лаунчер, совпадает с `ppid` start-записи probe:
I5 27708, N1 10724, N2 7732 (`--continue`), N3 25092 (`--resume 684d8e75-...`).
Для N5 (`--continue --model haiku`, без флага) лаунчер печатал только хвост, argv
взят из tool_use-команды, привязка к `probe_log_N5.jsonl` через
`CCTG_PROBE_SCENARIO=N5` и строку `not in --channels list` в `debug_N5.log`.

```
claude --dangerously-load-development-channels server:probe --model haiku
claude --resume 684d8e75-12f4-49a0-83cd-f706b8686468 --dangerously-load-development-channels server:probe --model haiku
claude --continue --dangerously-load-development-channels server:probe --model haiku
claude --continue --model haiku
claude -p --dangerously-load-development-channels server:probe --output-format stream-json --verbose --max-turns 4 --model haiku "<prompt>"
claude -p --resume 684d8e75-... --dangerously-load-development-channels server:probe --model haiku --max-turns 2 "<prompt>"
```

Баннер дословно (`screen_I5_banner.txt`, `screen_N2_banner.txt`, `screen_N3_banner.txt`):

```
Channels (experimental) messages from server:probe inject directly in this session · restart without
--dangerously-load-development-channels to stop
```

Без флага этой плашки на экране нет (`screen_N5_banner.txt`).

Экран `/mcp` снят только в fresh-прогоне I4 (`screen_I4_mcp.txt`,
`screen_I4_afterprompt.txt`, фрагмент):

```
    User MCPs (~\.claude.json)
  > probe · √ connected · 1 tool
  ...
  Probe MCP Server
  Status:           √ connected
  Config location:  ~\.claude.json
  Capabilities: tools
  Tools: 1 tool
```

**С флагом `/mcp` про channel ничего не пишет**: сервер выглядит как обычный
MCP с `Capabilities: tools`. Экран `/mcp` без флага, а также при `--resume` и
`--continue` не снимался, поэтому утверждение «`/mcp` одинаков в обоих случаях»
не проверено. Повторные прогоны в этом fix-раунде не делались (причина в разделе
«Уборка»). На вывод для hub это не влияет: раз даже с поднятым каналом `/mcp`
его не показывает, состояние «канал есть / канала нет» из UI не вытащить, оно
должно определяться самим агентом.

---

## 2. Ключевой факт: в headless `-p` канал не поднимается вообще

Самый весомый результат спайка. Он противоречит наивному ожиданию, что флаг
работает одинаково в обоих режимах.

Наблюдение (`debug_M1c.log`, `debug_M2.log`):

- probe спавнится и подключается: `MCP server "probe": Successfully connected (transport: stdio)`,
  его инструмент виден как `mcp__probe__reply`;
- строки `Channel notifications registered` нет;
- строк `Channel notifications skipped: ...` нет вообще ни для одного сервера,
  то есть подсистема каналов в `-p` не инициализируется, а не «инициализируется
  и отказывает»;
- исходящие `notifications/claude/channel` от probe не дают ни ответа, ни ошибки,
  ни записи в транскрипте: `grep -c PROBE-INBOUND <transcript>` = 0
  (сессии 8b45992a-..., 2591280b-..., 682bba42-...);
- `notifications/claude/channel/permission_request` в `-p` не приходит ни разу.

То же для `-p --resume` (`debug_M2.log`, `probe_log_M2.jsonl`): сессия корректно
продолжается (`result.session_id` тот же 684d8e75-..., ответ HEADLESSRESUME), но
канал не регистрируется.

**Последствие для hub:** фаза 6 плана (headless resume мёртвых сессий, TASK-019)
не может использовать канал для двусторонней связи. Поднятая через
`claude -p --resume` сессия — одноразовый прогон: промпт передаётся аргументом
или stdin, ответ забирается из `--output-format stream-json`, а `cctg agent` в
этом процессе висит как обычный MCP-сервер без единого канального сообщения.
Маршрутизацию на нём строить нельзя.

---

## 3. `--resume` и `--continue` наблюдались, канал поднимается полностью

Какой прогон каким флагом запущен, видно по сырому stdout лаунчера
(`evidence_from_transcript.txt` раздел 1): N2 это `--continue`, pid 7732, N3 это
`--resume 684d8e75-...`, pid 25092; те же pid стоят в `ppid` probe-логов.

`--continue` (`probe_log_N2.jsonl`, `screen_N2_*.txt`):

- probe стартовал с `CLAUDE_CODE_SESSION_ID = 684d8e75-12f4-49a0-83cd-f706b8686468`
  — тот же id, что у продолжаемой сессии, новый не заводится;
- баннер про channels на месте;
- inbound PROBE-INBOUND-N2IIII-T50 и -T95 доставлены, модель на каждое сама
  начала ход и вызвала `reply`;
- permission relay сработал на обычном Bash: probe получил
  `{request_id: "tcmbm", tool_name: "Bash", description: "Create a test file with echo command", input_preview: "{ \"command\": \"echo perm-test > perm_test.txt\", ... }"}`,
  ответил allow, команда выполнилась. `input_preview` приходит **строкой**
  (JSON-подобный текст), а не объектом: парсить его как объект нельзя.

`--resume <id>` (`probe_log_N3.jsonl`, `screen_N3_*.txt`): то же самое, session id
сохраняется, inbound -T30 и -T100 доставлены, relay сработал.

Вывод: тема в Telegram переживает `--resume`/`--continue` естественно — id сессии
не меняется, значит реестр hub `session_id -> topic` попадает в ту же тему без
дополнительной логики. Открытый вопрос из CLAUDE.md «как ведёт себя
`--dangerously-load-development-channels` при `claude --resume`» закрыт.

---

## 4. Доставка inbound и формат тега

Транскрипт сессии содержит ровно то, что обещает доменный файл:

```
<channel source="probe" probe="1" tag="T40">
PROBE-INBOUND-I5GGGG-T40
```

`source` — имя сервера, остальные ключи — наши `meta` один в один. Подтверждено в
сессиях 58a03120-... (4 совпадения) и 684d8e75-... (по 4 на прогон).

Сообщение канала само запускает ход, даже если пользователь в этот момент ничего
не вводил. На экране это блок `probe: <текст>` и затем `Called probe`.

Важная оговорка: в сессии, где пользователь ещё ни разу не отправил промпт, хода
не было. Прогон I3 (190 с, три inbound на 3/30/60 с): в debug-логе все три
`notifications/claude/channel: PROBE-INBOUND-I3EEEE-...` приняты, но транскрипт не
создан и `reply` не вызван. Как только в сессии был хотя бы один пользовательский
ход (I5, N1, N2, N3) — каждое канальное сообщение отрабатывалось. Для hub: только
что запущенная и ни разу не использованная сессия может проглотить сообщение без
ответа. Ещё один аргумент буферить на своей стороне.

Степень уверенности (QA): вывод держится на одном прогоне I3 без снимка экрана.
Отсутствие транскрипта само по себе ничего не доказывает: в ранних прогонах запись
транскрипта была выключена унаследованным `CLAUDE_CODE_CHILD_SESSION=1`. Считать
это гипотезой; практический вывод (буферить у себя) от неё не зависит.

---

## 5. Permission relay

| request_id | tool_name | прогон |
|---|---|---|
| cgcii, kkdoj, iadmd | mcp__probe__reply | I5 |
| tcmbm | Bash | N2 |
| krxyh, jxfiy | mcp__probe__reply | N2 |
| fddxi, hozwy | mcp__probe__reply | N3 |
| fdqmc | Bash | N4 |

`request_id` — 5 строчных букв, буквы `l` ни в одном нет. Формат запроса
`{request_id, tool_name, description, input_preview}`, ответ
`{request_id, behavior: "allow"}` — как в доменном файле. После allow инструмент
выполнялся.

Оговорка по охвату: команды, которые Claude Code считает read-only (`netstat -an`,
`echo` в headless), выполняются без запроса вообще — relay тут ни при чём, просто
разрешение не спрашивается. Чтобы увидеть relay, нужна команда с побочным
эффектом (запись в файл) при manual mode.

---

## 6. User-scope регистрация: per-project consent НЕ требуется — подтверждено

Проверено в заведомо новой папке `%TEMP%\cctg_probe_dir`, которой до этого не было
в `~/.claude.json` (проверено кодом перед запуском: `folder already known to
claude: False`). Сырой вывод этой проверки (17:46:50Z, за 6 с до запуска N1)
сохранён в `evidence_from_transcript.txt` раздел 2; проверка строже точного
ключа: любой ключ `projects`, содержащий `cctg_probe_dir`.

Уточнение fix-раунда: доверенного предка в конфиге сейчас нет
(`check_trust_ancestors_output.txt`: `~` есть, но `hasTrustDialogAccepted=False`,
`~/AppData/...` отсутствуют), так что отсутствие trust-диалога не объясняется
унаследованным доверием родительской папки. Почему диалога не было, не выяснено.

Первый запуск `claude --dangerously-load-development-channels server:probe` в этой
папке (`screen_N1_startup.txt`, снимок на 8-й секунде):

- нет диалога доверия к папке;
- нет диалога consent для MCP-сервера;
- сразу видна плашка про channels;
- probe спавнится с cwd = новая папка и работает (`probe_log_N1.jsonl`).

Запись при `--scope user` кладётся в top-level `mcpServers` файла `~/.claude.json`:

```json
"probe": {"type": "stdio", "command": "python", "args": ["<path>/probe_channel_server.py"], "env": {}}
```

Инвариант домена подтверждён: user-scope сервер работает в любой папке без
per-project consent. Проектный `.mcp.json` не нужен.

---

## 7. Вложенный `claude -p`: найден риск лишней регистрации в hub

Сценарий N4: интерактивная сессия с каналом (684d8e75-...) выполняет через свой
Bash-тул `bash nested.sh`, внутри которого
`claude -p --dangerously-load-development-channels server:probe`.

`probe_log_N4.jsonl`:

```
start pid 15020 ppid 17440 sid 684d8e75-12f4-49a0-83cd-f706b8686468 entry cli      <- родитель
start pid  5884 ppid  2200 sid f26f0436-22eb-432e-bfa7-d490bb891da9 entry sdk-cli  <- вложенный
stdin_eof pid 5884
```

1. Вложенный запуск действительно молча поднимает второй экземпляр нашего
   сервера: user-scope регистрация грузится в `-p` без всякого вопроса. Это ровно
   тот риск, что стоял в приёмке.
2. Экземпляр объявляет свой собственный `CLAUDE_CODE_SESSION_ID` (f26f0436-...),
   не родительский. Для hub, который матчит агента по этому env, он выглядит как
   самостоятельная сессия — именно так и появилась бы лишняя тема.
3. Но канальной регистрации он не получает: `-p` не инициализирует подсистему
   каналов (раздел 2). За всё время жизни вложенного экземпляра ноль
   `notifications/claude/channel`, ноль `permission_request`, затем EOF.

Итог (риск, а не доказательство безопасности): вложенный `claude -p` поднимает
второй экземпляр агента со своим session id и `CLAUDE_CODE_ENTRYPOINT=sdk-cli`.
Канала у него нет, inbound до него не дойдёт. Но `cctg agent` в этом процессе
всё равно попытается зарегистрироваться в hub с этим новым id, и без отдельной
логики hub заведёт под него слот и тему. Значит hub обязан распознать вложенность
(контракт TASK-003: обход дерева процессов и реестр `claude_pid -> session_id`) и
приклеить такую регистрацию к родителю или отклонить. Что hub реально сделает с
таким handshake, этот спайк не проверял: hub ещё не написан.

Признаки, которые видны в env агента (наблюдение, не готовое правило):

- `CLAUDE_CODE_ENTRYPOINT`: `cli` у интерактивной сессии, `sdk-cli` у любого
  `claude -p` (обе start-записи выше, `probe_log_M1*.jsonl`). **Вложенность он не
  отличает**: headless resume, который hub сам запустит кнопкой Resume
  (TASK-019), тоже будет `sdk-cli`, но это законная сессия своего слота.
  Поэтому одного `ENTRYPOINT` для решения мало.
- `CLAUDE_PID` в env MCP-сервера наследуется, а не выставляется его claude: у
  родителя N4 (запуск с очищенным env) он `null`, у вложенного `"17440"`, то
  есть pid **родительского** интерактивного claude (он же `ppid` первой
  start-записи), а не вложенного (ppid 2200). В M1/M1b/M1c/M2 это `15320`,
  claude implementer'а, из которого они запускались. Наличие `CLAUDE_PID` у
  агента похоже на признак «запущен из другой сессии Claude Code», но проверено
  только на этих прогонах.

Оговорка: сам вложенный прогон в N4 упал с `Error: Input must be provided either
through stdin or as a prompt argument when using --print` — промпт потерялся при
передаче через два слоя шелла. Спавн probe и объявление session id произошли до
этой ошибки, так что вывод от неё не зависит. Тот же результат независимо виден в
прогонах M1/M1b/M1c: они сами по себе вложенные `claude -p` (entry sdk-cli),
запущенные из живой сессии.

---

## 8. Поведение без флага: тихий drop

Прогон N5, `claude --continue --model haiku` без флага, probe остаётся
зарегистрированным user-scope.

- probe спавнится и подключается как обычный MCP-сервер;
- в debug-логе: `MCP server "probe": Channel notifications skipped: server probe
  not in --channels list for this session`;
- баннера про channels нет;
- probe отправил два inbound (N5LLLL-T25, -T55) — `grep -c N5LLLL <transcript>` = 0
  (транскрипт удалён при уборке; сохранённые следы: в `screen_N5_final.txt` и
  `debug_N5.log` строки `N5LLLL` нет ни одной, а входящих после `tools/list` в
  `probe_log_N5.jsonl` ноль). Ни ошибки в ответ, ни записи в транскрипте.
- permission relay без флага **не проверен**: в N5 модель только печатала
  `NOFLAG`, действия, требующего разрешения, не было. Ноль `permission_request`
  в `probe_log_N5.jsonl` поэтому ничего не доказывает. Ожидание (не наблюдение):
  relay тоже выключен, раз сервер «not in --channels list».

Для hub: «нет канала» — состояние, которое агент обязан детектировать сам, и
определить его можно только по факту молчания. Claude Code не отвечает на
`notifications/claude/channel` ничем: ни ack, ни JSON-RPC error. Значит
`cctg agent` не узнает из протокола, жив канал или нет. Практический вывод: агент
сообщает hub «канал заявлен, подтверждения нет», hub показывает тему в состоянии
«сессия есть, канал не поднят» и не считает исходящие доставленными.

---

## 9. Точная команда запуска MVP и alias

Решение пользователя: обёртки `cctg run` не будет. Запуск руками или через alias.
Это принятое решение, а не отложенный вопрос.

Разовая регистрация, один раз на устройство:

```sh
claude mcp add --scope user cctg -- cctg agent
```

Команда запуска сессии с каналом:

```sh
claude --dangerously-load-development-channels server:cctg
```

С `--resume` / `--continue` флаг просто дописывается, всё работает (раздел 3):

```sh
claude --resume <session-id> --dangerously-load-development-channels server:cctg
claude --continue --dangerously-load-development-channels server:cctg
```

Alias для `~/.bashrc` (Git Bash):

```sh
alias ccc='claude --dangerously-load-development-channels server:cctg'
```

PowerShell-профиль:

```powershell
function ccc { claude --dangerously-load-development-channels server:cctg @args }
```

`ccc --resume <id>` и `ccc --continue` работают как есть.

Флаг скрытый: в `claude --help` его нет (`claude --help 2>&1 | grep -i -A3 channel`
вернул пустой вывод, сырой результат в `evidence_from_transcript.txt` раздел 3),
но команда принимается без ошибки. Ориентироваться на help нельзя, только на плашку.

---

## 10. Побочные наблюдения (в приёмку не входили, но важны)

**`CLAUDE_CODE_CHILD_SESSION=1` отключает сохранение транскрипта.** Первые
интерактивные прогоны не оставляли ни одного `.jsonl`, пока на экране не нашлось
`Transcript saving is off - inherited CLAUDE_CODE_CHILD_SESSION marker`
(`screen_I4_banner.txt`). Переменная наследуется из сессии-родителя. Для hub это
значит: сессия, запущенная из другой сессии, может вообще не иметь файла
транскрипта, и `/brief` c `/full` по ней отдать нечего. Наличие файла надо
проверять, а не считать гарантированным.

**Encoded cwd заменяет и подчёркивание.** Папка `cctg_probe_dir` дала каталог
`~enc-AppData-Local-Temp-cctg-probe-dir`. В CLAUDE.md перечислены только
`:`, `\`, `/` и пробел. Расхождение с записанным фактом, важно для парсера пути
транскрипта. См. PCTX_PROPOSALS.md.

**`CLAUDE_PID` не всегда выставлен.** В интерактивных прогонах с очищенным
окружением probe видел `CLAUDE_PID: null`: своему MCP-серверу Claude Code её не
ставит, она приходит по наследству. Если она есть, это pid claude-предка, а не
того claude, что спавнил сервер (раздел 7). Связку агента и хука на неё
завязывать нельзя.

**`CLAUDE_CODE_ENTRYPOINT`** разделяет режимы: `cli` у интерактивной сессии,
`sdk-cli` у `claude -p` (любого, не только вложенного, см. раздел 7).

**Интерактивный старт бывает очень медленным.** В прогоне I2 между стартом
процесса и `session.start: raised` прошло около 85 секунд (`debug_I2.log`), в I3 и
остальных — меньше секунды. Один раз из семи. Причина не установлена. Если hub
будет ждать регистрации агента по таймауту, таймаут должен быть щедрым.

---

## 11. Воспроизведение

Скрипты (все в `scratch/`, оставлены намеренно):

- `probe_channel_server.py` — probe-сервер;
- `selftest_probe.py` — проверка probe без Claude Code, 8/8 PASS;
- `run_interactive_scenario.py` — интерактивный прогон по таймлайну;
- `type_into_console.py` — ввод в чужую консоль через AttachConsole +
  WriteConsoleInputW; SendKeys/AppActivate по pid не сработал (сырой вывод
  попытки I2: `ACTIVATE_FAILED`, `send_keys rc: 1`, транскрипта нет —
  `evidence_from_transcript.txt` раздел 4);
- `read_console_screen.py` — снимок экранного буфера чужой консоли через
  ReadConsoleOutputCharacterW; так сняты баннер и `/mcp`;
- `launch_interactive_probe.py` — ранняя версия лаунчера, из TASK-003;
- `inspect_user_config.py` — безопасный отчёт по `~/.claude.json`;
- `redact_scratch.py` — вычистка e-mail из снимков;
- `redact_home_paths.py` (fix-раунд) — замена домашнего пути во всех формах на
  `~` / `~enc`;
- `extract_transcript_evidence.py` (fix-раунд) — выжимка сырых выводов лаунчера,
  pre-launch проверки, `--help` и AppActivate из транскрипта implementer'а в
  `evidence_from_transcript.txt`;
- `check_trust_ancestors.py` (fix-раунд) — есть ли доверенный предок у папки
  consent-теста;
- `cleanup_probe_project_key.py` (fix-раунд) — удаление ключа
  `projects[<temp folder>]`; запуск заблокирован, см. «Уборка».

`run_interactive_scenario.py` в fix-раунде научен писать `run_<label>_argv.txt`
(точный argv, cwd, nonce, start/end UTC, pid, exit status) до запуска claude.
Повторных прогонов с ним не было.

Ограничение метода ввода: длинный промпт (больше одной экранной строки) уходит в
консоль как вставка, и Enter его не отправляет. Короткие промпты, примерно до 60
символов, отправляются нормально. Из-за этого вложенная команда в N2/N3 не ушла,
пришлось выносить её в `nested.sh` (N4).

## Уборка

**Утечка probe в чужую сессию (найдено QA).** Пока probe был зарегистрирован в
user scope, уже запущенная интерактивная сессия пользователя в другой папке
подняла его как обычный MCP-сервер (без флага канала), получила от него два inbound
(молча выброшены) и держит процесс вместе с `mcp__probe__reply`.
`claude mcp remove` уже запущенные экземпляры не гасит. Доказательство: вторая
start-запись без `stdin_eof` в `probe_log_default.jsonl` и `Win32_Process`
(python probe, родитель claude.exe этой сессии). Процесс не убивался
оркестратором: сессия чужая, решение за пользователем. Следствие для TASK-011:
`cctg agent`, зарегистрированный в user scope, спавнится в КАЖДОЙ сессии
устройства, включая запущенные без флага; агент должен работать и в режиме
"нет канала" (регистрироваться в hub, не падать, не спамить).

- `claude mcp remove --scope user probe` выполнено;
- `claude mcp list` больше не содержит probe, `grep -c probe_channel_server ~/.claude.json` = 0;
- top-level `mcpServers` вернулся к `web-reader, web-search-prime, zai-mcp-server, zread`;
- временная папка `%TEMP%\cctg_probe_dir` и её транскрипты удалены (fix-раунд:
  папки нет);
- симлинк `latest`, созданный `--debug-file`, удалён;
- e-mail аккаунта вычищен из всех снимков, ключей и токенов в захватах не найдено;
- fix-раунд: домашний путь во всех формах заменён на `~` / `~enc`
  (`redact_home_paths.py`), filename-only grep по `scratch/` на все варианты
  даёт 0 файлов, все строки `*.jsonl` по-прежнему валидный JSON.

**Не доделано:** в `~/.claude.json` остался ключ `projects["<home>/AppData/Local/Temp/cctg_probe_dir"]`,
который Claude Code сам завёл при запуске N1 в новой папке. Это единственное
вхождение `probe` в файле (count = 1). Удаление подготовлено
(`cleanup_probe_project_key.py`: load, `del` одного ключа, запись с indent=2,
ensure_ascii=False; round-trip файла через этот формат проверен как побайтно
идентичный), но запуск запрещён auto-mode classifier как изменение конфигурации
Claude Code. По той же причине не делались повторные прогоны: им нужен
`claude mcp add --scope user`, это та же запись в `~/.claude.json`. Нужна
команда пользователя:

```sh
python maw/tasks/in_progress/TASK-004/scratch/cleanup_probe_project_key.py AppData/Local/Temp/cctg_probe_dir
```

**Принятый остаток (решение пользователя 2026-09-22):** процесс probe в чужой живой сессии и ключ `projects[...cctg_probe_dir]` в `~/.claude.json` оставлены: пользователь активно работает в той сессии, оба остатка безвредны. Процесс умрёт вместе с сессией.

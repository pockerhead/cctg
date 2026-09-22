# TASK-003 — spike: идентичность вложенной claude-сессии

Дата: 2026-09-22. Claude Code 2.1.278, Windows 11 (26200), Git Bash, Python 3.10.
Весь код спайка временный, production-кода не писалось.

## Как снимали

Временный project-scope .claude/settings.json регистрировал scratch/probe_hook.py
на SessionStart, SessionEnd, Stop, SubagentStart, SubagentStop и на
PreToolUse/PostToolUse с matcher SubagentHandback. Хук на каждое событие дописывал
в $CCTG_PROBE_OUT одну redacted-JSON-строку: полный stdin, все CLAUDE_* переменные
окружения (значения с TOKEN/SECRET/KEY/SOCKET в имени заменены на "<redacted len=N>"),
полную цепочку ppid через CreateToolhelp32Snapshot. Домашний путь во всех написаниях
(бэкслеш-, слеш-, msys- и JSON-экранированная) заменён на ~.

Сценарии — 4 запуска claude -p (суммарно 8 сессий), отдельный настоящий интерактивный
старт и интерактивная orchestrator-сессия, чьи хуки подхватились сами:

| файл | что это |
|---|---|
| capture_00_selftest.jsonl | самопроверка хука на синтетическом stdin, claude не запускался |
| capture_A_toplevel.jsonl | настоящий top-level: env -u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CLAUDE_CODE_CHILD_SESSION ... claude -p |
| capture_B_nested.jsonl | top-level, из его Bash-тула claude -p (обычная вложенность, как у maw runner) |
| capture_C_mixed.jsonl | top-level: обычный вложенный + вложенный под env -u ... + субагент Explore |
| capture_D_nested_env_stripped.jsonl | top-level, из его Bash-тула "unset CLAUDE*; claude -p" в том же шелле |
| capture_E_interactive.jsonl | настоящий интерактивный старт: claude.exe в своей консоли (CREATE_NEW_CONSOLE), окружение предварительно очищено от всех CLAUDE_* — launch_interactive.py |
| capture_unknown.jsonl | интерактивная orchestrator-сессия, см. побочные факты; даёт SubagentHandback |

Разбор: dump_capture.py <файл>, прогон контракта: analyze_detect_parent.py
(результат сохранён в detect_parent_report.txt). Анализатор, кроме захватов,
проигрывает синтетические кейсы на ветки контракта, которых в захватах не было.

Важная оговорка про топологию. Все запуски claude -p (A/B/C/D) делались из Bash-тула
сессии-имплементера через обёртку `env -u ...` — то есть физически они тоже вложенные,
а не самостоятельные top-level процессы (см. transcripts/6-implementer.jsonl).
Интерактивный старт E запускался из Bash-тула этой сессии-фиксера. Полностью
отвязанного от claude запуска в захватах нет; что это меняет — в факте 3.

## Факт 1 — что видит хук в окружении

Четыре переменные, о которых спрашивала задача, видны хуку всегда и во всех снятых
сценариях — top-level -p, вложенный -p, интерактивный старт:

    CLAUDECODE=1, CLAUDE_CODE_CHILD_SESSION=1, CLAUDE_CODE_SESSION_ID, CLAUDE_PID

Остальной набор CLAUDE_* в целом тот же, но одинаковым его называть нельзя:

    CLAUDE_CODE_ENTRYPOINT, CLAUDE_CODE_MESSAGING_SOCKET, CLAUDE_CODE_MESSAGING_TOKEN,
    CLAUDE_CODE_SESSION_ATTENDED, CLAUDE_ENV_FILE, CLAUDE_PROJECT_DIR

плюс наблюдаемая вариативность: CLAUDE_CODE_EXECPATH есть только у вложенных сессий
(capture_C_mixed.jsonl записи 2 и 5, capture_D записи 2) и отсутствует у запускавших их
сессий (capture_C запись 1, capture_D запись 1) и у интерактивного старта
(capture_E_interactive.jsonl). Переменная CLAUDE появляется не везде; CLAUDE_EFFORT
видна в интерактивной сессии. Полный claude_env_all писался только начиная со
сценариев C/D/E, в A/B его нет, поэтому утверждать что-то про полный набор в A/B
нельзя. Ни в одном сценарии вариативность не затрагивает четыре ключевые переменные.

- CLAUDE_CODE_SESSION_ID в окружении хука — всегда id той сессии, которая этот хук
  и запустила. Во всех 8 сессиях env == stdin.session_id. Переменной, несущей id
  родителя, в окружении нет вообще.
- CLAUDE_PID — pid того же claude-процесса, что и в цепочке ppid. Тоже всегда свой,
  не родительский.
- CLAUDE_CODE_CHILD_SESSION=1 стоит и у настоящего top-level (сценарий A). Это не
  признак вложенности, а признак "я дочерний процесс claude-сессии". Для детекта
  вложенности бесполезна.
- CLAUDECODE=1 — то же самое: есть у всех, включая top-level.
- CLAUDE_CODE_SESSION_ATTENDED: 0 у -p, 1 у интерактивной. CLAUDE_CODE_ENTRYPOINT:
  sdk-cli у -p, cli у интерактивной. Это различает headless и интерактив, но не
  вложенность.
- Настоящий интерактивный SessionStart снят отдельно (сценарий E): claude.exe поднят
  в своей консоли с окружением, из которого предварительно убраны все CLAUDE_*.
  Хук всё равно увидел CLAUDECODE=1, CLAUDE_CODE_CHILD_SESSION=1,
  CLAUDE_CODE_SESSION_ID=1e087ca8… (== stdin.session_id), CLAUDE_PID=28764 (== pid
  claude.exe в цепочке), ATTENDED=1, ENTRYPOINT=cli. То есть очистка окружения ничего
  не даёт: сессия проставляет свои значения заново. В stdin интерактивного
  SessionStart дополнительно есть scratchpad_dir и model, которых у -p нет;
  session_id/transcript_path/cwd/source — те же.
- Дочерний claude перезаписывает CLAUDE_CODE_SESSION_ID и CLAUDE_PID своими
  значениями. Проверено и когда переменные унаследованы (B, C шаг 1), и когда они
  явно сняты через unset перед запуском (D): результат одинаковый, claude всё равно
  проставляет свои.

## Факт 2 — основной признак вложенности из CLAUDE.md опровергнут

Правило "в окружении хука CLAUDECODE=1, а CLAUDE_CODE_SESSION_ID не равен stdin
session_id, значит вложенный, и родитель известен" не срабатывает никогда.
Условие ложно во всех снятых случаях, включая заведомо вложенные B/C/D и
интерактивный старт E с предварительно очищенным окружением.
Единственная строка в detect_parent_report.txt, где правило сработало, — синтетическая
запись самопроверки с выдуманным session_id, то есть ложное срабатывание на мусоре.

Практический вывод: правило нельзя оставлять единственным. Как первую дешёвую проверку
держать можно (в будущей сборке поведение может измениться), но опираться на него нельзя.

## Факт 3 — ppid-fallback: работает, но с оговоркой

Процесс-дерево родителя несёт. Цепочка вложенного хука выглядит так (сценарий D):

    python(hook) <- bash <- bash <- claude(16024, своя сессия)
                 <- bash <- bash <- bash <- claude(7100, родитель) <- env.exe

Второй сверху claude-процесс — это родитель, и его pid совпадает с CLAUDE_PID, который
родительский SessionStart-хук уже записал. То есть карта claude_pid -> session_id,
наполняемая из SessionStart, закрывает случай перезаписанного окружения.

Результат прогона контракта по всем захватам (detect_parent_report.txt):

- 3 из 3 сессий, запущенных из Bash-тула БЕЗ обёртки env (B, C шаг 1, D вложенная),
  дали nested с верным родителем;
- интерактивный старт E дал nested_unknown_parent: обход нашёл живой claude-предок
  15320 (сессия, из чьего Bash-тула его и запустили), но в реестре этого файла записи
  про него нет. Классификация по дереву верна, родитель просто не зарегистрирован;
- 5 сессий помечены top_level, и вот тут поправка к первой редакции findings.

Поправка. Все пять «top-level» запусков на самом деле были запущены из Bash-тула
другой claude-сессии через обёртку `env -u ...` (transcripts/6-implementer.jsonl),
то есть физически они вложенные. Вердикт top_level для них — не успех детекта, а
следствие обрыва цепочки: у всех пятерых последний элемент это env.exe, выше которого
обхода нет. Значит правильная формулировка результата такая: там, где цепочка целая,
родитель находится (4 случая из 4), а там, где цепочка обрывается, вложенная сессия
неотличима от top-level (5 случаев из 5). Настоящего, ни от чего не порождённого
top-level запуска в захватах нет вообще, и его top_level-вердикт остаётся
непроверенным (хотя он тривиально следует из правила 3: нет claude-предков — нет
родителя).

Доказательство механизма обрыва. Обход идёт вверх, пока ParentProcessId находится в
снапшоте процессов. В сценарии D зафиксировано прямо: ppid_chain_truncated_at=35316,
то есть родитель env.exe(32968) к моменту срабатывания хука в снапшоте отсутствовал.
Для A/B/C поле ppid_chain_truncated_at ещё не писалось, так что там тот же механизм —
обоснованный вывод по совпадающей форме цепочки, а не отдельно снятый факт. Какой
именно процесс умирал над env.exe (промежуточный bash Bash-тула), остаётся гипотезой:
в данных есть только pid, которого уже нет.

Обратное подтверждение — сценарий E: там между claude и предком стоит живой python
(launch_interactive.py блокируется в ожидании), цепочка не рвётся и родительский
claude виден полностью. То есть решает не тип обёртки, а живость промежуточных
процессов в момент хука.

Практический вывод не меняется, но становится жёстче: любой короткоживущий процесс
в цепочке делает вложенную сессию неотличимой от top-level, и она получит лишнюю тему
в форуме. Для боевого сценария (maw runner зовёт claude -p из Bash-тула напрямую)
цепочка целая — это подтверждают B, C шаг 1 и D.

## Контракт

    detect_parent(hook_input, env, process_tree) -> Option<SessionId>

Вход: разобранный stdin хука, окружение процесса хука, снапшот процессов хоста.
Побочное состояние — реестр hub claude_pid -> session_id, наполняемый из SessionStart
и очищаемый на SessionEnd (обязательно: Windows переиспользует pid).

Порядок:

1. env.CLAUDE_CODE_SESSION_ID задан и не равен hook_input.session_id
   -> вложенный, родитель = registry[env.CLAUDE_PID], иначе само значение переменной.
   Наблюдений срабатывания нет. Оставлять только как дешёвую страховку на будущее.
2. Обход process_tree вверх от процесса хука. Пропустить первый claude-процесс,
   чей pid равен env.CLAUDE_PID (это наша собственная сессия). Следующий claude-предок
   и есть родитель -> registry[pid].
   - предок найден и есть в реестре -> Some(parent_session_id);
   - предок найден, в реестре нет -> вложенный, но родитель неизвестен. Не выдавать None:
     это не top-level, иначе будет создана лишняя тема.
3. claude-предков нет -> None, то есть top-level.

Прогон синтетических кейсов на ветки, которых не было в захватах
(analyze_detect_parent.py, раздел synthetic replay):

| кейс | вердикт |
|---|---|
| CLAUDE_PID отсутствует, цепочка вложенная | nested, родитель найден (пропускается первый claude по позиции) |
| claude-предок есть, в реестре нет | nested_unknown_parent |
| CLAUDE_PID протух/переиспользован, не совпадает ни с одним узлом цепочки | nested, но родителем объявляется СВОЯ ЖЕ сессия — баг |
| цепочка оборвана ниже родительского claude | top_level (ложный) |
| правило 1 сработало (env session != stdin session) | nested, родитель из реестра |

Замечания к реализации:

- Шаг 2 должен возвращать три состояния, а не Option: TopLevel, Nested(SessionId),
  NestedUnknownParent. Option<SessionId> из формулировки задачи схлопывает третье
  состояние в None и тем самым в "top-level", что даёт лишнюю тему. Предлагаю в
  TASK-012 расширить возвращаемый тип.
- Протухший CLAUDE_PID опасен: если значение переменной не совпадает ни с одним
  claude в цепочке, шаг 2 не пропускает собственный процесс и возвращает
  собственную же сессию как родителя. Обязательная страховка: если найденный
  родитель равен hook_input.session_id, считать, что это мы сами, и продолжать обход.
- "claude-процесс" на Windows определяется по имени образа claude.exe. На других
  платформах имя будет другим, проверка нужна отдельная.
- Обрыв цепочки неотличим от настоящего top-level. Частичная страховка: hub может
  придержать создание темы на короткий тайм-аут, если в той же папке прямо сейчас есть
  живая сессия с открытым Bash-вызовом. Это предложение, кодом не проверялось.

## Факт 4 — субагенты: фактические поля

SubagentStart (stdin):

    session_id, transcript_path, cwd, prompt_id, agent_id, agent_type, hook_event_name

плюс scratchpad_dir в интерактивной сессии (в -p его нет).

SubagentStop (stdin):

    session_id, transcript_path, cwd, prompt_id, permission_mode, agent_id, agent_type,
    hook_event_name, stop_hook_active, agent_transcript_path, last_assistant_message,
    background_tasks, session_crons

плюс scratchpad_dir и effort: {level} в интерактивной сессии.

transcript_path — транскрипт родителя, agent_transcript_path — транскрипт субагента,
реально существующий файл вида

    ~/.claude/projects/<enc-cwd>/<parent-session-id>/subagents/agent-<agent_id>.jsonl

Рядом лежит agent-<agent_id>.meta.json с полезным содержимым:

    {"agentType","description","toolUseId","spawnDepth","requestShape","requestNonInteractive"}

spawnDepth даёт глубину вложенности субагента напрямую.

Чем наполнен итоговый отчёт субагента — зависит от того, вызвал ли он SubagentHandback:

- Субагент Explore внутри claude -p (сценарий C) SubagentHandback не вызывал
  (в его jsonl инструмента нет, хуки PreToolUse/PostToolUse не стрельнули).
  Полный ответ пришёл в SubagentStop.last_assistant_message:
  "The directories directly under crates/ are: cctg and transcript."
- Субагент Explore в интерактивной сессии (capture_unknown.jsonl) SubagentHandback
  вызвал. Отчёт лежит в PreToolUse.tool_input.message (и там же в PostToolUse):

        tool_name: "SubagentHandback"
        tool_input: {"message": "2+2 = 4."}
        tool_response: {"success": true, "message": "Report delivered to your caller."}

  Поля agent_id и agent_type в этом событии есть, так что отчёт сразу привязывается
  к субагенту без корреляции по tool_use_id.

То есть проектное правило подтверждено, но с уточнением: вызов SubagentHandback не
универсален. Брать отчёт надо так: если по agent_id был PreToolUse SubagentHandback —
берём tool_input.message, иначе SubagentStop.last_assistant_message.

## Факт 5 — SubagentStop от внутренних агентов (подтверждено)

В интерактивной сессии на каждый вызов Bash-тула прилетал SubagentStop с
agent_type: "" и last_assistant_message, равным человекочитаемому описанию команды
(например "Dumping capture_A_toplevel.jsonl records"). Это внутренние агенты
Claude Code, не субагенты пользователя. SubagentStart для них не стреляет.

Надёжный фильтр: считать субагентом только тот agent_id, для которого раньше был
SubagentStart. Дополнительный сигнал: в background_tasks такого шумового события
лежит запись настоящего субагента
({"id","type":"subagent","status","description","agent_type"}), по ней же можно вести
список живых субагентов темы.

## Побочные факты (пригодятся TASK-012/015)

- .claude/settings.json подхватывается на лету. Уже запущенная интерактивная сессия
  начала исполнять хуки сразу после создания файла, без рестарта. Это удобно для
  установки cctg, но и риск: правка файла меняет поведение живых сессий.
- Команда хука на Windows исполняется через Git Bash (bash.exe в цепочке ppid), не
  через cmd.exe.
- CLAUDE_PROJECT_DIR передаётся в хук — можно не выводить корень проекта из cwd.
- CLAUDE_ENV_FILE указывает на ~/.claude/session-env/<session-id>/sessionstart-hook-0.sh;
  имя каталога содержит id сессии. Ещё один источник id, но появляется не во всех событиях.
- SessionEnd.reason во всех -p запусках был "other", не clear/exit.
- Stop и SessionEnd несут prompt_id; Stop несёт last_assistant_message и
  background_tasks, SessionEnd — нет.
- Каталог сессии для субагентов (<session-id>/subagents/) появляется только когда
  субагент реально стартовал.

## Что осталось непроверенным

- Запуск claude, не порождённый никакой другой claude-сессией (из проводника, из
  автозапуска). Все захваты сняты изнутри pipeline, см. поправку в факте 3.
- Поведение при claude --resume (задача этого не требовала).
- Переиспользование pid: реестр claude_pid -> session_id без очистки на SessionEnd
  рано или поздно склеит разные сессии. Очистка не тестировалась.
- Linux/macOS: имя процесса и устройство цепочки ppid другие, всё выше — про Windows.

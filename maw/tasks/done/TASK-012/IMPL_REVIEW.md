# IMPL_REVIEW — TASK-012 (cctg hook)

## 1. Verdict

**NEEDS_WORK**: все acceptance criteria в их буквальной формулировке выполнены и подтверждены тестами и живым замером. Но правило родителя в `proctree::lineage` ищет claude-предка только по имени образа `claude.exe`. На этом же хосте под этим именем работает Claude Desktop (Electron, 13 процессов). Он же везёт свой Claude Code (`%APPDATA%\Claude\claude-code\2.1.280\claude.exe`). Отсюда ложный `parent_claude_pid`, а hub TASK-011 любое `Some` считает вложенностью и тему не создаёт.

## 0. Disconfirmation

Контрпример, записанный до ревью: "SessionEnd при hub, который принимает TCP и молчит, выходит за 1.5 с, или таймаут POST не покрывает какую-то фазу (connect / write / чтение status line)".

Проверка: `scratch/crev/blackhole.ps1`, вывод в `scratch/crev/blackhole.out`. Там fake HOME с `device.env`, listener, который делает accept и ничего не отвечает, а хук запускается через Git Bash `bash -c "cctg.exe hook SessionEnd < fixture"`, как shell-form хук в Claude Code. Пять прогонов: 562–593 мс по стене, exit 0, stdout пуст, в stderr одна фиксированная строка `hook event not delivered event="session_end" error=hub did not answer within 500ms`, нет ни секрета, ни session id. В коде `tokio::time::timeout` оборачивает весь обмен: connect, включая DNS для `host:port`, write и `read_until` (`hook.rs:296-318`). **Контрпример не подтвердился.**

Второй контрпример (нашёлся по ходу ревью): **"top-level сессия, у которой выше собственного claude стоит процесс с образом `claude.exe`, но это не Claude Code"**. **Подтвердился**, см. issue 1.

## 2. Confirmed correct

- Сборка и тесты на `CARGO_TARGET_DIR` вне репо: `cargo test --workspace --offline -j 2` дал 277 passed, 0 failed, 1 ignored. `cargo clippy --workspace --all-targets -- -D warnings` и `cargo fmt --check` чистые (`scratch/crev_build.out`). `hook_cli` прогнан 5 раз подряд, 5/5 зелёные, ~1.07 с.
- Код совпадает с проверенным `REF` (`scratch/reviewer2/ws`) побайтно с точностью до CRLF. Единственное отличие — задокументированная правка теста в `hook.rs:791-794`, runtime-код не тронут. Затронуты ровно 18 файлов, `wire.rs`, `hub/*`, `agent.rs` не менялись.
- Зависимости: добавлен только `windows-sys 0.61` (cfg(windows)), он уже был в lockfile через tokio (`Cargo.lock` +1 строка). Нет teloxide, rmcp и прочего вне списка.
- `exit 0` всегда: `main.rs:36-44`. Там panic hook с фиксированной строкой, `tokio::spawn`, JoinError игнорируется, `process::exit(0)`. Tracing пишет в stderr (`main.rs:52-56`), в stdout не пишет никто.
- Stdin: отдельный поток, 300 мс, лимит 8 MiB, при превышении вход дропается целиком, а не обрезается (`hook.rs:104-116`). Зависший открытый stdin покрыт `hook_cli::an_open_silent_stdin_does_not_hold_the_hook`.
- Узкая десериализация: `Input` с `#[serde(default)]` (`hook.rs:134-156`). Текст промпта из `UserPromptSubmit` не уходит (тест `each_event_carries_its_fields_and_nothing_else` проверяет точные множества ключей). `background_tasks` не уходит.
- `source` читается только в ветке `SessionStart` (`hook.rs:174-181`), его отсутствие не ошибка (`source_is_optional_and_read_only_from_session_start`).
- Фильтр внутренних агентов: пустой `agent_type` даёт Skip (`hook.rs:244-247`). Typed `SubagentStop` без пути, с пустым путём или без `agent-<id>.jsonl`/`.meta.json` тоже Skip (`hook.rs:203-206`, `internal_agents_are_dropped`).
- Handback: `PreToolUse`/`PostToolUse` проходят только при `tool_name == SubagentHandback`, непустом `agent_id` и наличии `tool_input.message` (`hook.rs:214-226`). Текст режется до 128 KiB по границе символа. Даже при `\u`-экранировании каждого символа (x6) тело остаётся < `MAX_HOOK_BODY` = 1 MiB.
- Дерево процессов опрашивается только для `SessionStart`/`SessionEnd` (тест считает вызовы). `SessionEnd` несёт только собственный pid (`hook.rs:182-185`, `nesting_comes_from_the_lineage`).
- Правило own/parent (`proctree.rs:58-94`) соответствует PLAN_FINAL: own — ближайший `claude`, кроме `node` с pid == `CLAUDE_PID` ниже него. Parent — следующий `claude` выше с другим pid. Env-fallback срабатывает только при чужом `CLAUDE_CODE_SESSION_ID` и pid ≠ own. Цепочки TASK-003 (B, D, E) воспроизведены в тестах. Stale `CLAUDE_PID` на wrapper-е и на дальнем claude own не сдвигает. После `/clear`, если env id вдруг устарел, guard `env_claude_pid != claude_pid` не даёт назначить себя родителем.
- Unsafe ToolHelp (`proctree.rs:172-205`): хэндл проверен на `INVALID_HANDLE_VALUE`, `dwSize` выставлен, цикл заканчивается на `Process32NextW == 0`. `CloseHandle` вызывается ровно один раз, между Create и Close нет ранних выходов и паник. `szExeFile` режется по первому NUL с fallback на полную длину. В `ancestors` есть защита от циклов и глубина 64.
- Device config (`device.rs:62-98`): process env имеет приоритет, файл читается через `dotenvy::from_path_iter` в память, `set_var` нет. Ошибка dotenvy сворачивается в `Err(())` без содержимого, `ConfigProblem` выводится фиксированным текстом. `Secret` в Debug не раскрывается (`a_bad_secret_is_named_but_not_echoed`).
- `canonical_cwd`: `canonicalize` + снятие `\\?\` и `\\?\UNC\`, fallback на исходную строку. Путь `\\?\Volume{…}` оставлен как есть (`device.rs:143-166`).
- `docs/hook-settings.json`: 7 ключей, shell form `cctg hook <Event>`, matcher `SubagentHandback` только на `PostToolUse`, нет секретов, путей и адресов (`settings_snippet_registers_every_event_without_secrets_or_paths`).
- Фикстуры обезличены: `~` вместо home, `~enc-…` вместо encoded-cwd.

## 3. Issues

### 1. major — `crates/cctg/src/proctree.rs:65,79-84,97-99`: родитель определяется только по имени `claude.exe`, под которым на Windows живёт и Claude Desktop

Проверено на этом хосте (`Get-CimInstance Win32_Process`):
- 13 процессов `claude.exe` из `C:\Program Files\WindowsApps\Claude_2.7032.0.0_x64__…\app\claude.exe`, это Electron-приложение Claude Desktop: main-процесс и дочерние renderer/utility;
- Desktop сам поставляет Claude Code: `%APPDATA%\Claude\claude-code\2.1.280\claude.exe`, рядом каталог `claude-code-sessions`.

Последствия:
- (a) Если Claude Code запущен из Claude Desktop (вкладка Code или терминал, открытый из Desktop), цепочка выглядит так: `cctg -> bash -> claude.exe (Claude Code) -> … -> claude.exe (Desktop)`. `lineage` вернёт `parent_claude_pid = <pid Desktop>`, а `registry.rs:562-585` превратит это в `SlotOrParent::Parent(None)`: сессия без темы, молча. Не проверено, выполняет ли Claude Code внутри Desktop user-scope хуки из `~/.claude/settings.json`. Transcripts с `entrypoint: claude-desktop` в `~/.claude/projects` на хосте нет, значит Desktop-сессии здесь либо не запускались, либо пишутся в другое место. Вероятность реальна, но не доказана.
- (b) Оборванная цепочка + переиспользование pid. На этом хосте живьём виден обрыв: `claude.exe(36120) <- bash.exe(2368) <- [15580 мёртв]`. Если мёртвый pid занял новый процесс, `ancestors` пойдёт по нему дальше. Electron Desktop постоянно порождает и завершает дочерние `claude.exe`, поэтому шанс, что чужой pid окажется `claude.exe`, выше, чем для обычного бинаря. Результат тот же: top-level сессия без темы. Проверку creation time план отложил (OPEN_DECISIONS п.3), но риск от Desktop в плане не учтён: там рассматривался только node.

Предлагаемое исправление, минимальное и только в device-коде: подтверждать, что кандидат в родители — Claude Code. Например, для найденного по имени кандидата сделать `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` и отбрасывать путь под `\WindowsApps\Claude_` или `\AnthropicClaude\` (feature `Win32_System_Threading` в windows-sys, один лишний syscall только на SessionStart). От (b) заодно помогает сравнение `creation time` родителя и ребёнка через `GetProcessTimes`: родитель обязан быть старше. Минимум, если фикс откладывают: записать оба сценария в "Известные риски" и в PCTX, плюс тест-фиксация на цепочке `…, claude.exe(own), explorer.exe, claude.exe(desktop)`, чтобы поведение было осознанным.

### 2. minor — `crates/cctg/src/device.rs:70-74`: пустая переменная в process env затеняет `device.env`

`std::env::var(name).ok()` для `CCTG_HUB_SECRET=""` даёт `Some("")`, поэтому `or_else` к файлу не обращается, а фильтр пустых превращает значение в `NoSecret`. Пустой `CCTG_HUB_HOOK_ADDR` так же молча откатывается к дефолту, а не к значению из файла. Исправление: фильтровать пустые значения до `or_else` (`.ok().filter(|v| !v.trim().is_empty())`).

### 3. minor — `crates/cctg/src/hook.rs:193-196`: `SubagentStart` с typed `agent_type` без файлов не фильтруется

Критерий 7 требует отбрасывать события с `agent_type`, равным имени `--agent` сессии. Для `SubagentStop` это покрыто проверкой файлов, для `SubagentStart` проверки нет. Если `SubagentStart` вообще стреляет для главного потока `--agent`-сессии, что не проверено, hub зарегистрирует фантомного субагента, у которого никогда не будет Stop. Файлов на момент Start ещё может не быть, так что тот же фильтр сюда не переносится. Достаточно записать это в известные риски или проверить при живой установке.

### 4. minor — Linux-ветка не собрана

`proctree::linux`, `parse_stat` без `allow(dead_code)` и `/proc/sys/kernel/hostname` не компилировались: на хосте установлен только target `x86_64-pc-windows-msvc`. Код простой, по чтению ошибок нет, но это не доказательство. Предложение: `cargo check --target x86_64-unknown-linux-gnu` в CI или в задаче второго устройства.

## 4. Missing coverage

- `proctree`: цепочка с не-Claude-Code процессом `claude.exe` выше собственного claude (Desktop). Сейчас поведение не зафиксировано ни тестом, ни документом.
- `device::load` с пустой переменной в process env и значением в файле (issue 2).
- `hook::build` без `hook_event_name` во входе: принимается, и это правильно, но тестом не закреплено. Тест `source_is_optional…` косвенно покрывает только `SessionStart` без имени.
- CLI-тест `UserPromptSubmit` против молчащего hub: 300 мс проверены только юнит-тестом `post_timeout`, по стене не замерены.
- Хуки через Git Bash (shell form): в CI этого нет, есть только в моём probe (`scratch/crev/blackhole.ps1`, ~570 мс). Запас до 1.5 с большой, так что это информация, а не требование.

## 5. Nits

- `main.rs:52-56`: `tracing_subscriber::fmt()` пишет ANSI-цвета и timestamp в stderr хука. Claude Code показывает stderr хука в транскрипте или в verbose-режиме, так что для `hook` лучше `.with_ansi(false)` без времени. Это не безопасность, только читаемость.
- `hook.rs:289`: `expect("hook posts always serialize")` формально паника в runtime-пути. Она недостижима (строки и числа), а panic hook всё равно даёт фиксированную строку и exit 0.
- `proctree.rs:121`: проверка цикла `chain.iter().any` квадратичная, при глубине 64 это неважно.

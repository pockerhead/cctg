# PLAN — TASK-012: hook, события жизненного цикла и отчёт субагента

`T` = `maw/tasks/in_progress/TASK-012`. `REF` = `T/scratch/planner/ws`, копия рабочего дерева HEAD (`8cc81ea`) с полной реализацией. Она собрана и проверена: `cargo test --workspace` зелёный, clippy без предупреждений, `cargo fmt` применён. Исполнитель применяет один патч `T/scratch/planner/task012.patch` (18 файлов), сверяет sha256 и прогоняет проверки. Шаги ниже описывают, что в патче и почему, чтобы ревьюер мог спорить с решениями, а не с опечатками.

## 1. Understanding (что есть сейчас)

- `crates/cctg/src/main.rs:31-38`: `Command::Hook { event }` сейчас ничего не делает (`Command::Agent | Command::Hook { .. } => {}`). `event: String` принимает любое имя, clap падает только при отсутствии аргумента.
- `crates/cctg/src/hook.rs` (255 строк, TASK-010): только транспорт. `post(addr, secret, &HookPost, timeout)` делает один HTTP/1.1 POST по голому TCP, всё внутри одного `tokio::time::timeout`, успех только при полном `HTTP/1.1 204` (`parse_status`). `PostError` без секрета и без адреса. Повторов нет.
- `crates/cctg/src/wire.rs:320-420`: `HookPost { v, event_id, host, session_id, cwd, transcript_path, event }` и `HookEvent`: `SessionStart{source, claude_pid, parent_claude_pid}`, `SessionEnd{reason, claude_pid}`, `UserPromptSubmit{prompt_id}`, `Stop{prompt_id, last_assistant_message}`, `SubagentStart{agent_id, agent_type}`, `SubagentStop{agent_id, agent_type, agent_transcript_path, last_assistant_message}`, `SubagentHandback{agent_id, message}`. `MAX_HOOK_BODY` = 1 MiB. Premise challenge подтвердил, что все варианты проходят `decode_hook` (`wire.rs:711-725`). Контракт провода не меняется.
- `crates/cctg/src/hub/ingress.rs:400-457`: `serve_hooks` принимает `POST /v1/hook`, Bearer, дедуп по `event_id`, 204/400/401/503.
- `crates/cctg/src/hub/registry.rs:531-541, 553-664, 666-760`: хаб решает вложенность сам. `parent_claude_pid: Some(_)` означает вложенный запуск (родитель ищется в `pids` `<host>/<pid>`), `None` означает top-level. `SessionEnd.claude_pid`, не совпавший с pid живого запуска, игнорируется. `SubagentStart/Stop` с пустым `agent_type` хаб уже пропускает, `SubagentHandback` пока no-op. `folder_key` (`registry.rs:131-162`) только лексический и явно ждёт от устройства разрешённый путь.
- `crates/cctg/src/hub/config.rs:12-29, 113-130`: имена `CCTG_HUB_SECRET`, `DEFAULT_HOOK_LISTEN = 127.0.0.1:47292`; env-файл читается `dotenvy::from_path_iter` в память без `set_var`. Этот приём повторяем.
- `crates/cctg/tests/stdout.rs:3-22`: `cctg hook SessionStart` с пустым stdin должен выходить с 0 и пустым stdout. Остаётся зелёным.
- TASK-003 (`maw/tasks/done/TASK-003/scratch/FINDINGS.md`, `capture_*.jsonl`): реальные stdin всех событий, кроме `UserPromptSubmit`, и цепочки ppid. Вложенность видна только по дереву процессов. Хуки на Windows идут через Git Bash. `SubagentHandback` виден в `PreToolUse`/`PostToolUse` с `agent_id`/`agent_type`. 14 шумовых `SubagentStop` имеют `agent_type: ""`.

Проверено этим планировщиком (доказательства в `T/scratch/planner/`):
- `probe_timing.out`: ToolHelp-снимок ~450 процессов занимает 7.1 мс медиана, PowerShell `Get-CimInstance` 271 мс. Запуск текущего release `cctg.exe hook` 6.8 мс медиана, 215 мс холодный.
- `probe_internal_agents.out`: у всех 14 внутренних агентов из `capture_unknown.jsonl` нет ни `agent-<id>.jsonl`, ни `.meta.json`; у настоящего Explore есть оба.
- `measure_session_end.debug.out` (референсная debug-сборка): `SessionEnd` при закрытом порту 529-538 мс, при молчащем hub 526-532 мс, при отвечающем hub 18-39 мс.
- Доки хуков (https://code.claude.com/docs/en/hooks, 2026-09-23): stdout при exit 0 у `SessionStart` и `UserPromptSubmit` становится контекстом Claude, значит stdout должен быть пуст. У `SessionEnd` общий бюджет 1.5 с. `agent_type` приходит и у главного потока сессии с `--agent`. Есть exec form (`args`) и `shell`.
- tokio `Stdin` (https://docs.rs/tokio/latest/tokio/io/struct.Stdin.html): блокирующее чтение в отдельном потоке, отменить его нельзя, и shutdown runtime может повиснуть. Поэтому stdin читаем своим потоком, а процесс завершаем `std::process::exit(0)`.

## 2. Approach

Хук работает как чистая функция плюс тонкая оболочка ввода-вывода.

1. `hook::build(event, stdin, &Probe) -> Result<HookPost, Skip>` чистая: разбирает только нужные поля (`#[serde(default)]`), по имени события собирает `HookEvent`, отбрасывает внутренних агентов и чужие инструменты. Всё, что зависит от устройства (host, канонизация cwd, дерево процессов, наличие файлов), приходит через `Probe`. Поэтому все acceptance-проверки пейлоада делаются unit-тестами на фикстурах из TASK-003.
2. `hook::run(event)`: stdin в отдельном потоке (300 мс, максимум 8 MiB), затем `DeviceConfig::load()`, `build_here` (Probe на этом устройстве), затем `post` с таймаутом 500 мс. Любая неудача даёт одну строку фиксированного текста в stderr. `main` запускает `run` в `tokio::spawn`, чтобы паника не роняла процесс, ставит panic hook с фиксированным текстом (сообщение паники могло бы процитировать вход) и всегда вызывает `std::process::exit(0)`.
3. `proctree` (новый модуль): цепочка предков из одного ToolHelp-снимка (Windows, `windows-sys`) или из `/proc/<pid>/stat` (Linux), на прочих ОС цепочки нет. Чистая `lineage(chain, CLAUDE_PID, CLAUDE_CODE_SESSION_ID, stdin session)`: своя сессия это ближайший предок `claude(.exe)`, родитель это следующий `claude` выше. Правило TASK-003 "пропустить claude с pid == `CLAUDE_PID`" даёт тот же ответ, когда `CLAUDE_PID` верен, а при протухшем `CLAUDE_PID` не объявляет сессию своим же родителем. Результат `Lineage { claude_pid, parent_claude_pid }` ложится в существующие поля провода. Три состояния TASK-003 (TopLevel / Nested / NestedUnknownParent) различает хаб по своему `pids`, как уже сделано в TASK-011.
4. `device` (новый модуль, его переиспользует TASK-013): `DeviceConfig { secret, hook_addr, host }` из process env, затем `<home>/.cctg/device.env` (в память, без `set_var`). `host_name` берёт `CCTG_HOST`, затем `COMPUTERNAME` или `/proc/sys/kernel/hostname`, затем `unknown`. `canonical_cwd` вызывает `std::fs::canonicalize`, снимает `\\?\` и `\\?\UNC\`, при ошибке возвращает исходный путь.
5. Регистрация: `docs/hook-settings.json` (shell form `cctg hook <Event>`, без путей и секретов), проверяется тестом.

Почему так, а не иначе (альтернативы записаны в `log.jsonl` как `decision`):
- Один ToolHelp-снимок: 7 мс против 271 мс у PowerShell. `windows-sys 0.61.2` уже лежит в `Cargo.lock` через tokio, это официальные сырые биндинги Microsoft, берём только две фичи. `sysinfo` тяжёлый и собирает намного больше, чем ppid и имя.
- Один таймаут POST 500 мс на все события. Замер: на Windows connect к закрытому локальному порту длится до таймаута (SYN повторяется после RST). Значит остановленный hub стоит каждому хуку ровно этот таймаут, а `UserPromptSubmit` блокирует ввод. 500 мс укладывают `SessionEnd` в 1.5 с с запасом ~2.5x (замер 526-538 мс вместе со стартом процесса) и достаточны для hub по Tailscale.
- Фильтр внутренних агентов на устройстве: пустой `agent_type` (TASK-003) плюс для непустого `agent_type` проверка, что рядом с `agent_transcript_path` есть `agent-<id>.jsonl` или `.meta.json`. Это закрывает случай "`agent_type` = имя `--agent`" без знания об этом имени и без состояния SubagentStart (домен запрещает на него опираться). Хаб не трогаем.
- Handback-matcher в сниппете только на `PostToolUse`. Бинарник принимает и `PreToolUse`, но регистрация на оба события давала бы два события на отчёт.

## 3. Steps

Все пути от корня репозитория. Содержимое каждого файла лежит в `REF`, патч `task012.patch` собирается `T/scratch/planner/build_patch.py`.

1. **`crates/cctg/Cargo.toml`**: добавить
   `[target.'cfg(windows)'.dependencies] windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Diagnostics_ToolHelp"] }` с комментарием о причине. **`Cargo.lock`**: единственная строка `"windows-sys 0.61.2"` в зависимостях `cctg`. Новых скачиваний нет (`--offline` сборка прошла).
   Проверка: `cargo build -p cctg --offline`.

2. **`crates/cctg/src/device.rs`** (новый, ~230 строк с тестами):
   - константы `HOOK_ADDR_VAR = "CCTG_HUB_HOOK_ADDR"`, `HOST_VAR = "CCTG_HOST"`, `DEVICE_ENV = ".cctg/device.env"`; секрет под тем же именем, что у хаба (`hub::config::SECRET_VAR`), адрес по умолчанию `hub::config::DEFAULT_HOOK_LISTEN`;
   - `ConfigProblem { NoSecret, BadSecret, BadFile }` с фиксированным `Display` без значений;
   - `DeviceConfig { secret: Result<Secret, ConfigProblem>, hook_addr, host }`, `load()` никогда не падает, `from_vars(var)` чистая для тестов;
   - `read_env_file`: отсутствующий файл это пустой конфиг, ошибка разбора даёт `Err(())` без `dotenvy::Error` (урок TASK-008: ошибка dotenvy цитирует файл);
   - `canonical_cwd(&str) -> String` и `strip_verbatim` (`\\?\C:\x` → `C:\x`, `\\?\UNC\s\sh` → `\\s\sh`, прочие `\\?\` не трогает).
   Тесты: значения по умолчанию и переопределения, секрет не попадает в `Debug`/`Display`, снятие префиксов, канонизация (`.`/`..`, регистр на Windows, несуществующий путь остаётся как был), env-файл читается без `set_var`, битый файл даёт `Err(())`.

3. **`crates/cctg/src/proctree.rs`** (новый, ~420 строк с тестами):
   - `Proc { pid, name }`, `Lineage { claude_pid, parent_claude_pid }`, `current_lineage(env_pid, env_session, stdin_session)`, чистая `lineage(...)`, `ancestors(pid)` (не глубже 64, защита от циклов, остановка на ppid 0 или ppid == pid);
   - Windows: `Snapshot::take()` через `CreateToolhelp32Snapshot` / `Process32FirstW` / `Process32NextW`, один `unsafe` блок с SAFETY-комментарием, handle закрывается один раз. Linux: `/proc/<pid>/stat`, `parse_stat` берёт `comm` между первой `(` и последней `)`. Прочие ОС: `None`;
   - `is_claude`: `claude` или `claude.exe` без учёта регистра;
   - правило env (TASK-003, правило 1, ни разу не срабатывало): если `CLAUDE_CODE_SESSION_ID` не пуст и не равен stdin `session_id`, а обход родителя не нашёл, родитель это `CLAUDE_PID` (если он не равен своему pid).
   Тесты на цепочках из `capture_B/D/E` (top-level, вложенный, вложенный с очищенным env, интерактивный), протухший и отсутствующий `CLAUDE_PID`, exec form (хук прямой ребёнок claude), нет цепочки, `node.exe` по pid, правило env, имена, `parse_stat`, живая цепочка текущего процесса.

4. **`crates/cctg/src/hook.rs`**: транспорт (`post`, `parse_status`, `PostError`, существующие тесты) без изменений. Добавить:
   - `POST_TIMEOUT = 500 ms`, `STDIN_TIMEOUT = 300 ms`, `MAX_STDIN = 8 MiB`, `MAX_TEXT = 128 KiB` (даже полностью `\u`-экранированный текст оставляет тело меньше `MAX_HOOK_BODY`);
   - `run(event)`, `build_here` (синхронная, чтобы `Probe` с `&dyn Fn` не жил через `.await`, иначе E0277 в `tokio::spawn`), `read_stdin` (поток, `take(limit + 1)`, `recv_timeout`);
   - `Probe`, `Skip(&'static str)`, приватные `Input`/`ToolInput` (`#[serde(default)]`, только нужные поля);
   - `build`: пустой `session_id` или `hook_event_name`, отличный от аргумента, дают Skip. `SessionStart` берёт `source` и lineage; `SessionEnd` берёт `reason` и `lineage.claude_pid` (родитель не шлётся); `UserPromptSubmit` только `prompt_id` (текст промпта не уходит); `Stop` берёт `prompt_id` и `last_assistant_message` с обрезкой; `SubagentStart` требует непустые `agent_id` и `agent_type`; `SubagentStop` дополнительно проверяет `has_agent_files`; `PreToolUse`/`PostToolUse` только при `tool_name == "SubagentHandback"`, нужны `agent_id` и `tool_input.message`; прочие события дают Skip. Дерево процессов запрашивается только для `SessionStart`/`SessionEnd`;
   - `cap_text` режет по границе символа (`floor_char_boundary`, стабилен в 1.95) и ставит `…`.
   Модульный комментарий обновить: stdout всегда пуст, stderr только фиксированные тексты, ссылки на `docs/hook-settings.json` и `crate::device`.
   Тесты (`build_tests`): у каждого события ровно ожидаемые ключи верхнего уровня и объекта `event`, lineage спрашивается только у Start/End, вложенность пробрасывается, `source` необязателен и читается только в `SessionStart`, внутренние агенты (пустой тип, тип без файлов) отбрасываются, а агент только с `.meta.json` проходит, чужой инструмент и неполный handback пропускаются, все префиксы реального входа и мусор не дают паники, чужое или неизвестное событие пропускается, длинный текст обрезается и тело меньше 1 MiB.

5. **`crates/cctg/src/lib.rs`**: `pub mod device;` и `pub mod proctree;`.

6. **`crates/cctg/src/main.rs`**: ветка `Command::Hook { event }`: panic hook с `"cctg hook: internal error"`, `tokio::spawn(run)`, результат join игнорируется, затем `std::process::exit(0)`. `Command::Agent => {}` остаётся как было.

7. **`crates/cctg/tests/fixtures/hook/*.json`** (9 файлов): stdin из захватов TASK-003 (домашний путь уже `~`), собраны `T/scratch/planner/make_fixtures.py`. В `stop.json` mojibake заменён на `Ok.`. В `subagent_stop_internal.json` описание команды заменено нейтральным текстом, `background_tasks` пустой. `user_prompt_submit.json` синтетический: `UserPromptSubmit` в TASK-003 не снимался. Секретов и user id нет.

8. **`crates/cctg/tests/hook_cli.rs`** (новый, отдельный тестовый бинарник): бинарник запускается как Claude Code, stdin через pipe, `USERPROFILE`/`HOME` указывают на временный home с `.cctg/device.env`, hub настоящий (`ingress::serve_hooks`).
   - `every_event_reaches_the_hub`: 7 вызовов (6 событий плюс `PostToolUse` handback), каждый дошёл, `kind` и `host` верные, stdout пуст;
   - `a_silent_hub_keeps_session_end_well_inside_its_budget`: hub принимает и молчит, `SessionEnd` меньше 1.2 с по стене, exit 0, stdout пуст, в stderr нет секрета и входа;
   - `no_hub_listening_is_quiet_and_fast`: закрытый порт, то же для Start/End;
   - `broken_input_and_missing_config_exit_zero_quietly`: пустой, обрезанный и мусорный stdin, отсутствие конфига (есть фиксированное сообщение, нет входа);
   - `settings_snippet_registers_every_event_without_secrets_or_paths`: сниппет содержит ровно 7 ключей, команды `cctg hook <Event>`, matcher `SubagentHandback` только у `PostToolUse`, нет `CCTG_`, `secret`, `/`, `\\`, `:\`, `~`, `127.0.0.1`.

9. **`docs/hook-settings.json`** (новый): `hooks` для `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop` и `PostToolUse` (matcher `SubagentHandback`), тип `command`, shell form. Реальные `.claude/settings.json` не трогаются, хуки не регистрируются (`.claude/` в gitignore).

Применение и проверка (исполнитель):
```
git apply --check maw/tasks/in_progress/TASK-012/scratch/planner/task012.patch
git apply maw/tasks/in_progress/TASK-012/scratch/planner/task012.patch
bash maw/tasks/in_progress/TASK-012/scratch/planner/verify_hashes.sh      # 18 x OK
CARGO_TARGET_DIR=<%TEMP%\...> cargo test --workspace --offline -j 2       # см. scratch/planner/workspace_test.txt
cargo clippy -p cctg --all-targets --offline ; cargo fmt --all -- --check
```
`git apply --check` на текущем HEAD проходит. Хэши считаются по LF-байтам, скрипт снимает CR.

Соответствие acceptance:
| критерий | где доказано |
|---|---|
| пейлоад каждого события, ничего лишнего | `build_tests::each_event_carries_its_fields_and_nothing_else`, `hook_cli::every_event_reaches_the_hub` |
| недоступный hub: exit 0, пустой stdout, stderr без входа и секрета | `hook_cli::a_silent_hub_…`, `no_hub_listening_…` |
| вложенность и parent по правилу TASK-003 | `proctree::tests::*` на цепочках B/D/E, `build_tests::nesting_comes_from_the_lineage` |
| битый, пустой, обрезанный stdin | `build_tests::broken_input_…`, `hook_cli::broken_input_…`, `tests/stdout.rs` |
| `SessionEnd` меньше 1.5 с | `hook_cli::a_silent_hub_…` (< 1.2 с), замер `measure_session_end.debug.out` |
| сниппет без секретов и путей | `hook_cli::settings_snippet_…` |
| handback и поля SubagentStop, фильтр внутренних | `build_tests::each_event_…`, `internal_agents_are_dropped` |
| `source` только из SessionStart, необязателен | `build_tests::source_is_optional_…` |
| канонизация cwd | `device::tests::canonical_cwd_*`, `Probe.cwd` в `build` |
| `parent_claude_pid` только при claude-предке, отличном от своего | `proctree::tests` (`exec_form…`, `no_chain…`, `top_level…`) |
| `SessionEnd.claude_pid` из обхода дерева | `build` (`lineage.claude_pid`), `nesting_comes_from_the_lineage` |
| существующие тесты | `workspace_test.txt`: все зелёные |

## 4. Risk areas

- **Обрыв цепочки** (известная дыра TASK-003): короткоживущая обёртка между claude и вложенным claude делает вложенный запуск похожим на top-level, и он получит лишнюю тему. Дерево это не лечит. Боевой случай (Bash-тул держит шелл живым) работает.
- **Переиспользование pid в цепочке.** ToolHelp не даёт времени создания, поэтому если промежуточный процесс умер и его pid занял новый процесс, ppid может указать на чужую ветку. Для ложной вложенности чужая ветка должна содержать `claude.exe`. Сверка creation time (`OpenProcess` + `GetProcessTimes`) не реализована, см. вопрос 3.
- **Claude, установленный через npm** (`node.exe`, а не `claude.exe`): своя сессия находится по `CLAUDE_PID`, родителя по имени не найти, вложенные запуски выглядят как top-level. Нативная установка (как на этой машине) не затронута.
- **Остановленный hub стоит каждому хуку 500 мс**, включая `UserPromptSubmit`, который блокирует ввод. Это цена короткого таймаута на Windows (замер). Если это мешает, лечится отдельной задачей (например, быстрый пропуск после недавнего отказа, записанного в файл), не здесь.
- **DNS в `CCTG_HUB_HOOK_ADDR`**: `TcpStream::connect(&str)` резолвит имя в blocking-пуле, таймаут это покрывает, а зависший поток резолвера убивает `process::exit`. Для Tailscale лучше указывать ip:port.
- **Фильтр по файлам субагента** опирается на наблюдение (14 из 14 внутренних без файлов, 1 из 1 настоящий с файлами). Если Claude Code начнёт писать транскрипты внутренним агентам, шум пройдёт в hub. Хаб их только регистрирует, ничего не рендерит. Если перестанет писать `.meta.json` настоящим, `SubagentStop` с непустым типом без файлов потеряется. Второй риск ниже: `jsonl` у настоящего субагента есть к моменту `SubagentStop`.
- **Shell form через Git Bash** добавляет bash в цепочку и время старта. Без Git Bash Claude Code берёт PowerShell, и `cctg hook X` там тоже работает, если `cctg` в PATH. `cctg` должен быть в PATH, это условие установки (TASK позже).
- **`canonicalize` на сетевом или subst-диске** возвращает UNC-путь, а не букву диска. Хук и агент получат одинаковое написание (общий helper), но заголовок темы покажет последний компонент UNC-пути.
- **Большие ответы**: `last_assistant_message` и отчёт режутся до 128 KiB. Полный текст остаётся в транскрипте.

## 5. Open questions

1. Handback-matcher: только `PostToolUse` (принято) или `PreToolUse`? `PreToolUse` приходит раньше, но блокирует субагента до ответа хука. `PostToolUse` приходит только при успешной доставке отчёта. Если Pre нужен для надёжности, хабу придётся дедуплицировать по `agent_id`.
2. Exec form (`"command": "cctg", "args": ["hook", "SessionStart"]`) убирает Git Bash из цепочки и экономит старт. Непроверено, ищет ли Claude Code на Windows `cctg.exe` по PATH для exec form. Пока shell form.
3. Нужна ли сверка creation time предков (риск про переиспользование pid)? Это +1 `OpenProcess` на звено, `Win32_System_Threading`. Предлагаю отложить до реального случая.
4. Имя файла конфига устройства: `~/.cctg/device.env` (принято). На главном устройстве секрет лежит дважды: в `.env` хаба и в `device.env`. Можно ли хабу по умолчанию читать тот же файл, решать не здесь.

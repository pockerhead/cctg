# PLAN — TASK-013: agent, Channel MCP server over stdio

`T` = `maw/tasks/in_progress/TASK-013`. `REF` = `T/scratch/planner/ws`: копия дерева HEAD `3933240` с полной реализацией. Она собрана и проверена на этой машине: `cargo test --workspace` 305 passed / 0 failed / 1 ignored (`T/scratch/planner/workspace_test.txt`), `cargo clippy -p cctg --all-targets -- -D warnings` чисто, `cargo fmt --all -- --check` чисто. Исполнитель применяет один патч `T/scratch/planner/task013.patch` (13 файлов, +1666/−29), сверяет sha256 (`hashes.txt`, по LF-байтам) и прогоняет проверки. Шаги ниже объясняют, что в патче и почему, чтобы ревью спорило с решениями, а не с опечатками.

Живые наблюдения этой стадии (Claude Code 2.1.280, `--model haiku`, свой консольный процесс с очищенным env, сервер только через временный `--mcp-config`, в `~/.claude.json` ничего не регистрировалось):
- `T/scratch/clear_evidence.txt`, `probe_log_clear.jsonl`, `debug_CLEAR.log`, `screen_CLEAR_*.txt`: вопрос `/clear`.
- `T/scratch/live_hub.jsonl`, `debug_LIVE.log`, `screen_LIVE_*.txt`: ручной запуск референсного `cctg agent` против поддельного hub (`T/scratch/planner/fake_hub.py`).

## 1. Understanding

Что есть сейчас (HEAD `3933240`):

- `crates/cctg/src/main.rs:20-52`: подкоманда `Agent` есть, но `Command::Agent => {}` ничего не делает. `init_tracing` (58-72) пишет в stderr; для хука без ANSI и времени.
- `crates/cctg/src/agent.rs` (TASK-010): только hub-link. `spawn(LinkConfig) -> (Sender<AgentMsg>, Receiver<LinkEvent>)` (81-86), `run` переподключается с equal-jitter backoff 250 мс..30 с и заново шлёт `hello`+`register` (88-130), outbox 256 сообщений ждёт, пока линк лежит. Чтение хаба в отдельной задаче (213-231), как требует урок TASK-010 про `select!`.
- `crates/cctg/src/wire.rs:112-185`: `Register { session_id, host, cwd }`, `AgentMsg::{Hello, Register, Reply{text}, PermissionRequest}`, `HubMsg::{Registered, Rejected, Inbound{content, meta: BTreeMap}, PermissionVerdict{request_id, behavior}}`. `MAX_LINE` 1 MiB на строку линка. Новое необязательное поле не меняет `VERSION`.
- `crates/cctg/src/device.rs:52-92`: `DeviceConfig { secret, hook_addr, host }`, `load()` (env, потом `~/.cctg/device.env`, без `set_var`), `host_name` приватный и доступен через `DeviceConfig.host`; `canonical_cwd` (147-158) общий хелпер. Адреса agent-листенера hub в конфиге устройства нет.
- `crates/cctg/src/proctree.rs:95-160`: `current_lineage(env_pid, env_session, stdin_session)`; при `env_pid = None` своим claude считается ближайший предок `claude(.exe)` с проверкой пути образа.
- Hub: `hub/ingress.rs:135-242` принимает агента, отдаёт `AgentEvent::Registered{conn, register, to_agent}`. `hub/slots.rs:252-310`: `on_agent` привязывает соединение к `register.session_id` или держит в `pending` до SessionStart; `conns: HashMap<u64, (String, Sender<HubMsg>)>`. `hub/registry.rs:764-781`: `agent_connected` привязывает к любой известной сессии, в том числе вложенной. `pids: "<host>/<pid>" -> session` заполняется на SessionStart и чистится на SessionEnd (383-385, 652-655, 697-702). Маршрутизации Telegram-текста в агента и `Reply` в тему в hub пока нет (`slots.rs:282` "agent message not routed yet").
- `tests/stdout.rs:4-21`: `cctg agent` с закрытым stdin должен выйти 0 с пустым stdout.

Факты платформы, от которых зависит план:
- Channel-домен и TASK-004 FINDINGS: surface MCP, capabilities, форма permission relay, `input_preview` приходит строкой, в `-p` канал не поднимается совсем, user-scope сервер спавнится в каждой сессии, `CLAUDE_PID` в MCP-сервере унаследован от чужого claude.
- Документация channels (https://code.claude.com/docs/en/channels-reference): ключи meta только буквы, цифры, `_`, остальные молча выкидываются; `request_id` пять букв `a-z` без `l`, Claude Code принимает только выданный им id; с 2.1.234 permission request уходит только серверам, зарегистрированным как канал сессии; `description`/`input_preview` режутся Claude Code до 3500 code points на поле.
- MCP 2025-11-25, tools (https://modelcontextprotocol.io/specification/2025-11-25/server/tools): неизвестный инструмент и битый запрос это JSON-RPC ошибки (`-32602`), ошибки ввода и бизнес-логики это `result.isError: true`, чтобы модель могла исправиться.
- MCP 2025-11-25, lifecycle (https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle): сервер отвечает той же версией протокола, если поддерживает её; до `notifications/initialized` сервер не должен слать ничего, кроме ping и logging; stdio закрывается закрытием stdin.

Новое, наблюдено здесь:
1. **`/clear` не перезапускает MCP-сервер канала.** Один процесс probe (pid 25704) на всю сессию, в его env остаётся старый `CLAUDE_CODE_SESSION_ID` `d10b35d3-…`, после `/clear` сессия новая (`ec6e5724-…`, отдельный jsonl), Claude Code заново пишет `Channel notifications registered`, inbound T70 от старого процесса доставлен в новую сессию (тег `<channel source="probe13" …>` в `ec6e5724-….jsonl`), permission relay `dffmo → allow (matched pending)` работает. Значит без доработки после `/clear` hub видит агента на закончившейся сессии, а новая висит в «нет канала».
2. **Dev-канал принимает сервер из `--mcp-config`.** Живые проверки не требуют записи в `~/.claude.json`.
3. **Родитель MCP-сервера это его собственный claude.** ppid probe = pid запущенного claude; референсный агент зарегистрировался с `claude_pid: 15620` = pid запущенного `claude.exe` (`live_hub.jsonl`).
4. Ручной прогон референса (`live_hub.jsonl`, `debug_LIVE.log`): регистрация с каноническим cwd и хостом из конфига; inbound доставлен (`notifications/claude/channel: LIVE13-A…`), Claude вызвал `reply`, hub получил `reply{text:"pong-a"}`; permission relay для `mcp__cctg__reply` и для `Bash` прошёл через hub (`dxpca`, `hebcw`, оба `matched pending`); после `/clear` inbound B дошёл в новую сессию (`51b9f3b8-…`); вложенный `claude -p` с тем же `--mcp-config` (сессия `1b1d502d-…`, `entrypoint sdk-cli`) не открыл второго соединения с hub. Баннер каналов на 2.1.280 свёрнут в «1 more notice hidden», доказательство подъёма канала это строка `Channel notifications registered` в debug-логе.

## 2. Approach

Три части, каждая как можно меньше.

**A. Чистый протокол (`channel.rs`).** `Server` без IO: `on_line(&[u8]) -> Vec<Vec<u8>>`, `on_link(LinkEvent) -> Vec<Vec<u8>>`. Каждая выходная строка это `serde_json::to_vec` одного объекта плюс `\n`; serde экранирует переводы строк, поэтому одна запись = одна строка по построению. Ошибки с фиксированным текстом, вход не эхоится. Так все критерии протокола проверяются unit-тестами без процессов.

**B. Цикл агента (`agent.rs`).** stdin читается в отдельном std-потоке (блокирующее чтение нельзя отменить, tokio stdin держал бы остановку рантайма; тот же приём, что в хуке), кадры идут в mpsc. Один цикл `serve_channel` владеет stdout и пишет строки по одной с flush. Hub-link из TASK-010 используется как есть. Закрытие stdin завершает цикл, `main` делает `process::exit(0)`.

**C. `/clear` на стороне hub.** Агент сообщает pid своего claude (`Register.claude_pid`, необязательное поле, `VERSION` остаётся 1). Hub связывает соединение не только по env session id, но и по процессу: на SessionStart с `claude_pid` все соединения этого `host/pid` переезжают на живую top-level сессию этого pid; при регистрации со старым id закончившейся сессии берётся текущая сессия pid. Альтернатива (новый `HubMsg::Rebind`, агент переподключается с новым id) отвергнута: она требует `VERSION = 2` и всё равно требует от hub знать pid агента. Решение записано в `log.jsonl`.

**Вложенный запуск.** Агент с `CLAUDE_CODE_ENTRYPOINT=sdk-cli` (любой `claude -p`) отвечает по MCP, но к hub не подключается: в `-p` канала нет никогда (TASK-004), а exec-обёртки Git Bash рвут цепочку процессов и делают вложенный запуск похожим на top-level (урок TASK-012 QA). Hub-side защита второго уровня: `Registry::agent_connected` не привязывает агента к `Nested` сессии.

**Установка.** `cctg agent-install` печатает `claude mcp add --scope user cctg -- "<абсолютный путь к exe>" agent`. Не выполняет: запись в `~/.claude.json` остаётся шагом пользователя (классификатор блокирует её из субагентов, и это правильно).

Что сознательно не делается: рендер permission-кнопок в Telegram (TASK-014), стриминг ходов (TASK-016), маршрутизация текста темы в агента и `Reply` в тему (не закреплена ни за одной задачей, см. открытые вопросы), детект «флаг канала не передан» (из протокола невозможен, `initialize` одинаков с флагом и без, проверено по `probe_log_N5/I4`).

## 3. Steps

### Шаг 0. Предусловия
- `git status` чистый, ветка задачи `feature/agent-channel-server` (или текущая `feature/agent-channel`, если orchestrator уже на ней; не переключать без указания).
- Одна cargo-сборка за раз, target вне репо: `CARGO_TARGET_DIR=%TEMP%\cctg-task013-target`.

### Шаг 1. Применить патч
```bash
git apply --check maw/tasks/in_progress/TASK-013/scratch/planner/task013.patch
git apply maw/tasks/in_progress/TASK-013/scratch/planner/task013.patch
bash maw/tasks/in_progress/TASK-013/scratch/planner/verify_hashes.sh   # ожидается 13 x OK
```
`git apply --check` на HEAD `3933240` проходит. Хэши по LF (скрипт снимает CR, рабочее дерево на Windows в CRLF). Запасной путь: скопировать 13 файлов из `REF/<путь>` и прогнать `verify_hashes.sh`.

### Шаг 2. Что в патче, по файлам

1. **`crates/cctg/Cargo.toml`**: у tokio добавлена фича `io-std` (для `tokio::io::stdout`). Lockfile не меняется, новых крейтов нет.

2. **`crates/cctg/src/wire.rs`**: `Register` получает `#[serde(default)] pub claude_pid: Option<u32>` с комментарием про `/clear`. Тест `a_register_without_claude_pid_still_decodes` (старая строка без поля даёт `None`); в образце `agent_samples` поле заполнено. Все литералы `Register {..}` в тестах дополнены `claude_pid: None` (`agent.rs`, `hub/ingress.rs`, `hub/slots.rs`, `tests/ingress_logs.rs`, `tests/slots_logs.rs`).

3. **`crates/cctg/src/device.rs`**: `AGENT_ADDR_VAR = "CCTG_HUB_AGENT_ADDR"`, поле `DeviceConfig.agent_addr` с умолчанием `DEFAULT_AGENT_LISTEN` (`127.0.0.1:47291`), строка в doc-комментарии про `device.env`, проверки умолчания и переопределения в `defaults_and_overrides`. Хост агента берётся из того же `DeviceConfig.host`, cwd из того же `device::canonical_cwd`, что у хука (критерий TASK-011).

4. **`crates/cctg/src/channel.rs`** (новый, ~330 строк кода + тесты). `Server::new(Hub)`, где `Hub::Off(NoHub)` или `Hub::Link(Sender<AgentMsg>)`, `NoHub::{Headless, NoSession, NoConfig}` с фиксированными текстами.
   - `on_line`: пустая строка → ничего; не JSON → `-32700`, `id: null`; не объект (в том числе batch) → `-32600`; `method` строка без `id` → уведомление; с `id` строкой или числом → запрос; `id` другого типа → `-32600`, `id: null`; объект без `method`, но с `result`/`error` → молча (мы запросов не шлём); прочее → `-32600`.
   - Запросы: `initialize` → `protocolVersion` клиента (или `2025-06-18`, если не прислан), `capabilities: { tools: {}, experimental: { "claude/channel": {}, "claude/channel/permission": {} } }`, `serverInfo {name: "cctg", version}`, `instructions`. `tools/list` → один `reply` со схемой `{text: string}`, `required: ["text"]`, `additionalProperties: false`. `tools/call`: нет имени или чужое имя → `-32602`; `reply` с пустым/нестроковым `text` → `isError: true`; без hub → `isError: true` с причиной; `try_send` в outbox: ок и линк поднят → «Sent…», ок и линк лежит → «…queued…», очередь полна → `isError`. Всё остальное, включая `ping`, → `-32601` (закон домена: «every other method answers method-not-found»; TASK-004 и оба живых прогона: Claude Code шлёт только `initialize`, `notifications/initialized`, `tools/list`, `tools/call` и `permission_request`).
   - Уведомления: `notifications/initialized` включает отправку и сбрасывает удержанные; `…/permission_request` → проверка `request_id` (`^[a-km-z]{5}$`), поля режутся до 32 KiB, `AgentMsg::PermissionRequest` в outbox, id запоминается (не больше 64 открытых); прочие игнорируются.
   - `on_link`: `Up`/`Down` меняют состояние; `Inbound` → `notifications/claude/channel {content, meta}` через `channel_meta` (ключи вне `[A-Za-z0-9_]+` выбрасываются, не переименовываются, значения как есть); `PermissionVerdict` только для открытого id → `notifications/claude/channel/permission {request_id, behavior}`, повтор или чужой id выбрасывается. До `initialized` уведомления держатся в очереди на 64 (старейшее выпадает с warn).
   - Размеры: `reply` режется до 128 KiB по границе символа с `…` (даже полностью `\u`-экранированный влезает в `wire::MAX_LINE`), поля permission по 32 KiB (три экранированных поля тоже влезают). Иначе длинная строка закрыла бы линк на стороне hub (`TooLong`) и сообщение потерялось бы.

5. **`crates/cctg/src/agent.rs`**: новая doc-шапка модуля; добавлены `MAX_RPC_LINE` (8 MiB), `Frame::{Line, TooLong}`, `run_stdio()`, `link_plan()`, `read_frames()`, `skip_line()`, `serve_channel()`, `install_command()`. Существующий hub-link не менялся.
   - `run_stdio`: `DeviceConfig::load()`; `link_plan(env CLAUDE_CODE_SESSION_ID, env CLAUDE_CODE_ENTRYPOINT, &config)`: `sdk-cli` → `Headless`, нет/пустой id → `NoSession`, нет секрета → `NoConfig` (одна warn-строка), иначе `Register { session_id, host: config.host, cwd: canonical_cwd(current_dir), claude_pid: proctree::current_lineage(None, None, "").claude_pid }` и `spawn(LinkConfig { addr: config.agent_addr, .. })`. Env `CLAUDE_PID` не используется (в MCP-сервере он чужой).
   - `read_frames`: std-поток, `read_until` с `take(MAX_RPC_LINE)`; более длинная строка дочитывается до `\n` и выбрасывается, в цикл уходит `Frame::TooLong` (ответ `-32700`), синхронизация по строкам не теряется.
   - `serve_channel<W: AsyncWrite>(frames, output, hub, events)`: `select!` по кадрам и событиям линка (сами чтения в своих задачах, в `select!` только `recv`), каждый пакет строк пишется и flush-ится. Конец stdin → выход; outbox и events уходят вместе с `Server`, линк останавливается (семантика TASK-010).

6. **`crates/cctg/src/main.rs`**: `Command::Agent` ставит panic hook с фиксированной строкой `cctg agent: internal error` в stderr, запускает `tokio::spawn(agent::run_stdio())`, затем `process::exit(0)` (поток чтения stdin может ещё висеть). Новая подкоманда `AgentInstall` (`cctg agent-install`) печатает `install_command(canonical exe path)`. `init_tracing(plain)`: для хука и агента без ANSI и без времени (stderr агента Claude Code складывает в свой debug-лог). Тест разбора `agent-install`.

7. **`crates/cctg/src/lib.rs`**: `pub mod channel;`.

8. **`crates/cctg/src/hub/registry.rs`**: `agent_connected` привязывает только `TopLevel`-сессии; новые `live_session_of_pid(host, pid) -> Option<&str>` (живая top-level сессия процесса) и `is_live_top_level(session)`. Тесты `the_pid_of_a_cleared_process_names_the_new_session`, `the_agent_of_a_nested_run_is_never_bound`.

9. **`crates/cctg/src/hub/slots.rs`**: `conns: HashMap<u64, Conn { session, host, claude_pid, _to_agent }>`. `agent_session(&Register)`: env id, если это живая top-level сессия; иначе живая сессия `host/claude_pid`, если есть; иначе env id (ждёт в `pending`, как раньше). `follow_pid(host, pid)` в `on_hook` после каждого `SessionStart` с `claude_pid`: соединения этого процесса, привязанные к другой сессии или ждущие, отвязываются (`agent_disconnected`, чистка `pending`) и привязываются к новой (лог `agent follows its claude process to a new session`, только короткие id). В `Rig` добавлен `agent_of(conn, session, claude_pid)`. Тест `the_agent_follows_its_claude_process_through_clear`: после `SessionEnd(clear)` + `SessionStart(source=clear)` того же pid тема та же, иконка «нет канала» ни разу, последняя иконка «alive»; после разрыва линка регистрация со старым id снова даёт «alive».

10. **`crates/cctg/tests/agent_stdio.rs`** (новый): настоящий бинарник, настоящие pipes, `USERPROFILE`/`HOME` во временный каталог (никогда не читается `device.env` разработчика), чистый env. Hub, который принимает и сразу рвёт соединение, гарантирует warn в stderr во время сеанса. Проверки: 8 входных строк (initialize, initialized, tools/list, reply, неизвестный метод, мусор, пустая строка, tools/list) дают ровно 6 строк stdout, каждая JSON-объект с `jsonrpc: "2.0"`; в stdout нет `hub link`, `WARN`, `INFO` и секрета; в stderr есть `hub link` и нет секрета; закрытие stdin завершает процесс с кодом 0 меньше чем за 5 с. Второй тест: headless (`sdk-cli`), без секрета, без session id → те же 6 ответов, `reply` с `isError`, попыток подключения нет.

### Шаг 3. Не делать
- Не регистрировать `cctg` в `~/.claude.json` и не трогать реальные `settings.json`.
- Не менять `VERSION` линка, `hub/ingress.rs` (кроме литерала в тесте), планировщик и рендер Telegram.
- Не коммитить `target/`, `.cctg/`, `.env`.

## 4. Test plan

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task013-target'
cargo test --workspace --offline -j 2
cargo clippy -p cctg --all-targets --offline -j 2 -- -D warnings
cargo fmt --all -- --check
git diff --check
```
Ожидается 305 passed, 0 failed, 1 ignored; `git diff --stat` ровно 13 файлов.

| Критерий | Чем закрыт |
|---|---|
| initialize → initialized → tools/list → tools/call, по одному объекту на строку | `channel::tests::{initialize_declares_the_channel, tools_list_offers_reply_only, reply_goes_to_the_hub}`; бинарник: `agent_stdio::a_full_session_with_a_failing_hub_keeps_stdout_pure` |
| неизвестный метод `-32601`, битый ввод не валит сервер | `channel::tests::{bad_calls_are_answered_not_fatal, broken_input_gets_controlled_answers}` (все префиксы реального запроса, не-UTF-8, batch, id-объект), `agent::tests::frames_are_lines_and_an_oversized_line_is_skipped_whole`, `agent_stdio` |
| stdout без логов и паник, в том числе при недоступном hub | `agent_stdio::a_full_session_with_a_failing_hub_keeps_stdout_pure` (warn реально пишется в stderr во время сеанса, урок TASK-002), panic hook с фиксированным текстом, `tests/stdout.rs` |
| невалидный ключ meta отброшен, валидные байт в байт | `channel::tests::inbound_waits_for_initialized_and_keeps_valid_meta_byte_for_byte` (кавычки, `<>`, `\n`, `\t`, эмодзи, кириллица в значении; дефис, точка, пробел, кириллица, пустой ключ), `channel::tests::meta_keys`, `agent::tests::the_channel_relays_both_ways_and_survives_a_hub_restart` |
| разрыв и восстановление hub не завершают MCP, та же сессия регистрируется снова | `agent::tests::the_channel_relays_both_ways_and_survives_a_hub_restart` (во время простоя MCP отвечает, `reply` в очереди, после рестарта тот же `Register` и доставка), `agent::tests::the_agent_reconnects_and_registers_again_after_a_hub_restart` (TASK-010) |
| ручной запуск: баннер и inbound; вложенный запуск не регистрируется | живой прогон `T/scratch/live_hub.jsonl` + `debug_LIVE.log` (одна регистрация, inbound A и B доставлены, relay двух запросов, вложенный `sdk-cli` не подключался); `agent_stdio::headless_and_unconfigured_agents_answer_without_a_hub`; `registry::tests::the_agent_of_a_nested_run_is_never_bound`. Баннер на 2.1.280 свёрнут в «notices hidden», см. риски |
| cwd и хост тем же хелпером, что у хука | `run_stdio` использует `DeviceConfig.host` и `device::canonical_cwd`; живой прогон: `host: live13` (из `CCTG_HOST`), `cwd` канонический |
| `/clear` проверен наблюдением, перерегистрация по SessionStart(clear) | наблюдение: `T/scratch/clear_evidence.txt` (сервер не перезапускается); `slots::tests::the_agent_follows_its_claude_process_through_clear`, `registry::tests::the_pid_of_a_cleared_process_names_the_new_session` |
| команда установки с абсолютным путём | `agent::tests::install_command_quotes_the_absolute_path`, `main` разбор `agent-install` |
| Existing tests pass | весь workspace |

Мутации на `REF` (`T/scratch/planner/mutations.out.txt`), все убиты: убрать `follow_pid`; убрать pid-fallback при регистрации (второй половиной теста `/clear`); убрать пропуск `sdk-cli`; не фильтровать meta; привязывать агента к вложенной сессии.

Живая перепроверка для QA (по желанию, та же схема): собрать `cctg.exe`, скопировать во временную папку, `mcp.json` с `{"mcpServers":{"cctg":{"command":"<abs>/cctg.exe","args":["agent"],"env":{"CCTG_HUB_SECRET":"<синтетический>","CCTG_HUB_AGENT_ADDR":"127.0.0.1:47391","CCTG_HOST":"live13"}}}}`, `python T/scratch/planner/fake_hub.py 47391 <секрет> <лог> --inbound "25:…"`, затем `T/scratch/run_interactive_scenario.py` с `-- --mcp-config <mcp.json> --dangerously-load-development-channels server:cctg --model haiku --debug-file <лог>`. После прогона удалить временную папку и её каталог в `~/.claude/projects`.

## 5. Risk areas

- **Hub не маршрутизирует Telegram → агент и `Reply` → тема.** После этой задачи агент и линк готовы, но пользователь в Telegram ничего не увидит, пока кто-то не свяжет `updates::Inbound` в теме с `HubMsg::Inbound` в соединение текущей сессии слота и `AgentMsg::Reply` с отправкой в тему. Ни одна pending-задача это не называет (TASK-014 только permission, TASK-016 транскрипт, TASK-017 предполагает, что доставка есть). См. открытый вопрос 1.
- **Флаг канала не передан.** Агент регистрируется и сессия выглядит «alive», хотя inbound Claude Code молча выбросит (домен). Отличить из протокола нельзя. Частичный сигнал: `permission_request` приходит только зарегистрированным каналам (документация, 2.1.234+), но отсутствие запросов ничего не доказывает. Буфер на стороне hub (TASK-017) обязателен.
- **Привязка по pid.** npm-only установка (claude как `node.exe`): `current_lineage` не найдёт claude, `claude_pid = None`, после `/clear` сессия останется «нет канала» до рестарта Claude Code. Переиспользование pid: `pids` чистится на SessionEnd; при потерянном SessionEnd агент может временно привязаться к устаревшей сессии, `follow_pid` на SessionStart это исправляет.
- **Кратковременная иконка «dead» при `/clear`.** SessionEnd приходит раньше SessionStart, актор успевает показать «dead» (поведение TASK-011, тест это допускает). Не регресс, но видно пользователю.
- **`sdk-cli` как признак headless.** Наблюдение TASK-004 на 2.1.280. Если Claude Code переименует entrypoint или VS Code-расширение выставит `sdk-*`, агент молча уйдёт в «без hub». `reply` при этом явно говорит причину.
- **Версия протокола.** Эхо любой версии клиента. Если будущий MCP сделает обязательным что-то, чего мы не реализуем, Claude Code это не проверяет сейчас, но может начать.
- **Баннер.** На 2.1.280 с `--debug-file` баннер каналов свёрнут в «N more notices hidden»; критерий «ручной запуск подтверждает баннер» лучше проверять раскрытием уведомлений или строкой `Channel notifications registered`.
- **stderr агента в debug-логе Claude Code помечается `[ERROR]`** (`Server stderr: INFO registered with the hub`). Косметика; уровень логов агента не снижался, чтобы не прятать реальные проблемы линка.
- **Остатки в `~/.claude.json`:** два прогона в новых папках оставили ключи `projects["…/Temp/cctg13_clear_probe"]` и `projects["…/Temp/cctg13_live"]` (Claude Code создаёт их сам; `mcpServers` не менялся). Удаление это шаг пользователя, как в TASK-004.

## 6. Open questions

1. **Кто делает маршрутизацию текста темы в сессию и `reply` в тему?** Предложение: отдельная `full`-задача сразу после TASK-013 (или расширить TASK-014 до «inbound/outbound/permission end to end»). Без неё шаг 3 плана разработки («сообщение из темы дошло в сессию, ответ вернулся») не закрыт.
2. **Показывать ли hub «канал подтверждён» по первому `permission_request`?** Дёшево (агент может прислать флаг в новом необязательном поле), но сигнал редкий. Предлагаю не делать сейчас.
3. **`ping`.** Сейчас `-32601` по закону домена. MCP определяет `ping` как базовую утилиту; Claude Code его не слал ни в одном прогоне. Если PCTX-куратор согласен, разрешить пустой ответ на `ping` отдельной правкой домена.

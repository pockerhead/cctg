# PLAN_FINAL — TASK-013: agent, Channel MCP server over stdio

`T` = `maw/tasks/in_progress/TASK-013`. `REF` = `T/scratch/reviewer2/ws`: исправленная копия референса планировщика (`T/scratch/planner/ws`), собранная и проверенная на этой машине. Патч `T/scratch/reviewer2/task013.patch` (13 файлов, 2242 строки, sha256 файла патча `d6b9450664cee66558ceb1d51b2d58fa813cfd0e97101b0e2225d5df47592ce9`) применяется к текущему HEAD (`767c334`; код в `crates/`, `Cargo.*`, `docs/` не менялся с `3933240`). Хэши результата: `T/scratch/reviewer2/hashes.txt`, проверка: `T/scratch/reviewer2/verify_hashes.sh`. Старые `T/scratch/planner/task013.patch` и `hashes.txt` не использовать: там четыре дефекта, описанных ниже.

## 1. Summary

Подкоманда `cctg agent` становится Channel MCP-сервером по stdio: вручную написанный JSON-RPC 2.0 на `serde_json` (`channel.rs`, чистое состояние без IO), один писатель stdout, чтение stdin в отдельном потоке, переподключаемый TCP-линк к hub из TASK-010. Агент регистрируется в hub с `session_id` из env `CLAUDE_CODE_SESSION_ID`, хостом и каноническим cwd через общие хелперы хука (`device.rs`) и pid собственного claude (`Register.claude_pid`, новое необязательное поле, `wire::VERSION` остаётся 1). Headless (`CLAUDE_CODE_ENTRYPOINT=sdk-cli`), запуск без session id и без конфига отвечают по MCP, но к hub не подключаются. Наблюдение показало, что `/clear` не перезапускает MCP-сервер, поэтому hub переносит соединение агента на новую сессию того же `(host, claude_pid)` по `SessionStart`. `cctg agent-install` печатает команду регистрации в user scope с абсолютным путём exe и ничего не выполняет. По сравнению с референсом планировщика исправлено: `ping` отвечает `{}`, envelope проверяется строго (`jsonrpc: "2.0"`, структурные `params`), версия протокола согласуется по списку поддерживаемых вместо эха, повторный `permission_request` с открытым id не пересылается. Добавлен тест на `/clear` с обратным порядком хуков.

## 2. Implementation steps

### Шаг 0. Предусловия

- `git status` чистый. Ветка: текущая `feature/agent-channel` (orchestrator уже на ней), без указания не переключать.
- Cargo строго по одной сборке за раз. Target вне репозитория: `CARGO_TARGET_DIR=%TEMP%\cctg-task013-target`. Если debug-линковка падает с `LNK1104` или `D8050`, повторить с `CARGO_PROFILE_DEV_DEBUG=0` и `-j 1` (так воспроизводилось у обоих ревьюеров).
- Не читать `.env`, не обращаться к Telegram API, не регистрировать ничего в `~/.claude.json` и в реальных `settings.json`.

### Шаг 1. Применить патч и сверить хэши

```bash
git apply --check maw/tasks/in_progress/TASK-013/scratch/reviewer2/task013.patch
git apply maw/tasks/in_progress/TASK-013/scratch/reviewer2/task013.patch
bash maw/tasks/in_progress/TASK-013/scratch/reviewer2/verify_hashes.sh   # ожидается 13 x OK, exit 0
```

Хэши считаются по LF-байтам (скрипт снимает CR). Проверено: патч применён к `git archive HEAD` в чистый каталог, все 13 хэшей совпали, дерево `crates/` побайтно равно `REF/crates` (с точностью до CR).

Запасной путь, если `git apply` не проходит: скопировать 13 файлов из `REF/<путь>` поверх репозитория (список ниже, пути те же) и прогнать `verify_hashes.sh`. Никаких других файлов не копировать: остальное в `REF` совпадает с HEAD.

### Шаг 2. Что в патче, по файлам (для ревью; руками ничего не писать)

1. `crates/cctg/Cargo.toml`: у `tokio` добавлена фича `io-std` (нужна для `tokio::io::stdout`). Новых крейтов нет, `Cargo.lock` не меняется.
2. `crates/cctg/src/wire.rs`: `Register` получает `#[serde(default)] pub claude_pid: Option<u32>`. `VERSION = 1`. Тест `a_register_without_claude_pid_still_decodes`. Литералы `Register { .. }` в тестах дополнены `claude_pid: None` (`agent.rs`, `hub/ingress.rs`, `hub/slots.rs`, `tests/ingress_logs.rs`, `tests/slots_logs.rs`).
3. `crates/cctg/src/device.rs`: `CCTG_HUB_AGENT_ADDR` -> `DeviceConfig.agent_addr`, по умолчанию `127.0.0.1:47291`. Порядок загрузки прежний: env процесса, затем `~/.cctg/device.env`, без `set_var`. Хост и `canonical_cwd` те же, что у хука (критерий TASK-011).
4. `crates/cctg/src/channel.rs` (новый). `Server::new(Hub)`, `Hub::Off(NoHub::{Headless, NoSession, NoConfig})` или `Hub::Link(Sender<AgentMsg>)`.
   - `on_line`: пустая строка -> ничего. Не JSON -> `-32700`, `id: null`. Не объект (включая batch) -> `-32600`, `id: null`. Объект без `method`, но с `id` и `result`/`error` -> молча (мы запросов не шлём). `jsonrpc` не равен строке `"2.0"` (нет, `"1.0"`, число, `null`) -> `-32600`, `id: null`, сообщение не исполняется (в том числе `notifications/initialized` и `permission_request`). `params` есть, но не объект и не массив (число, строка, bool, `null`) -> `-32600`; id эхоится, если это строка или число, иначе `null`. `method` строка без `id` -> уведомление; с `id` строкой или числом -> запрос; `id` другого типа -> `-32600`, `id: null`. Прочее -> `-32600`.
   - Запросы. `initialize`: `params.protocolVersion` обязателен, непустая строка, иначе `-32602` с id запроса. Версия из `SUPPORTED_PROTOCOLS = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]` возвращается как есть, любая другая -> `LATEST_PROTOCOL = "2025-11-25"`. Ответ: `capabilities: { tools: {}, experimental: { "claude/channel": {}, "claude/channel/permission": {} } }`, `serverInfo { name: "cctg", version }`, `instructions`. `ping` -> `result: {}` в любой момент, в том числе до `initialized`. `tools/list` -> один инструмент `reply`, схема `{text: string}`, `required: ["text"]`, `additionalProperties: false`. `tools/call`: нет имени или чужое имя -> `-32602`; пустой или нестроковый `text` -> `isError: true`; без hub -> `isError: true` с причиной; `try_send` в outbox: линк поднят -> "Sent…", линк лежит -> "…queued…", очередь полна -> `isError: true`. Остальные запросы -> `-32601`.
   - Уведомления. `notifications/initialized` разрешает отправку и сбрасывает удержанное (очередь до 64, старейшее выпадает с warn). `notifications/claude/channel/permission_request`: `request_id` по `^[a-km-z]{5}$`; если такой id уже открыт, запрос не пересылается и не добавляется повторно; иначе поля режутся до 32 KiB, `AgentMsg::PermissionRequest` в outbox, id запоминается (до 64 открытых). Прочие уведомления игнорируются.
   - `on_link`: `Up`/`Down` меняют состояние. `Inbound` -> `notifications/claude/channel {content, meta}`, ключи meta вне `[A-Za-z0-9_]+` выбрасываются (не переименовываются), значения без изменений. `PermissionVerdict` только для открытого id -> `notifications/claude/channel/permission {request_id, behavior}`, id закрывается; повторный или чужой verdict выбрасывается.
   - Размеры: `reply` режется до 128 KiB по границе символа с `…`, поля permission по 32 KiB: даже полностью `\u`-экранированные они помещаются в `wire::MAX_LINE` (1 MiB).
   - Тексты ошибок фиксированные, вход не эхоится и не логируется.
5. `crates/cctg/src/agent.rs`: сохранённый hub-link TASK-010 без изменений плюс `MAX_RPC_LINE` (8 MiB), `Frame::{Line, TooLong}`, `run_stdio`, `link_plan`, `read_frames` (std-поток, `read_until` через `take`, слишком длинная строка дочитывается до `\n` и выбрасывается, в цикл уходит `TooLong` -> `-32700`), `serve_channel` (единственный писатель stdout, write + flush на каждую пачку; в `select!` только `recv`), `install_command`. `run_stdio`: `link_plan(env CLAUDE_CODE_SESSION_ID, env CLAUDE_CODE_ENTRYPOINT, config)`: `sdk-cli` -> `Headless`, нет или пустой id -> `NoSession`, нет секрета -> `NoConfig`; иначе `Register { session_id, host: config.host, cwd: device::canonical_cwd(current_dir), claude_pid: proctree::current_lineage(None, None, "").claude_pid }`. Env `CLAUDE_PID` не используется (в MCP-сервере он чужой или пустой).
6. `crates/cctg/src/main.rs`: `Command::Agent` ставит panic hook с фиксированной строкой `cctg agent: internal error` в stderr, ждёт `tokio::spawn(agent::run_stdio())`, затем `process::exit(0)` (поток stdin может висеть). `init_tracing(plain)`: для hook и agent в stderr без ANSI и времени. Новая подкоманда `agent-install` печатает `claude mcp add --scope user cctg -- "<canonical current_exe>" agent`. Тест разбора `agent-install`.
7. `crates/cctg/src/lib.rs`: `pub mod channel;`.
8. `crates/cctg/src/hub/registry.rs`: `agent_connected` привязывает только `TopLevel`; новые `live_session_of_pid(host, pid)` и `is_live_top_level(session)`. Тесты `the_pid_of_a_cleared_process_names_the_new_session`, `the_agent_of_a_nested_run_is_never_bound`.
9. `crates/cctg/src/hub/slots.rs`: `conns: HashMap<u64, Conn { session, host, claude_pid, _to_agent }>`. `agent_session(&Register)`: env id, если это живая top-level сессия; иначе живая сессия `host/claude_pid`; иначе env id (ждёт в `pending`). `follow_pid(host, pid)` после каждого `SessionStart` с `claude_pid` переносит соединения этого процесса на его живую top-level сессию (отвязка от старой, чистка `pending`). Маршрутизации `Reply`/`Inbound` в Telegram нет (TASK-021). Тесты: `the_agent_follows_its_claude_process_through_clear` (End -> Start) и новый `the_agent_follows_its_claude_process_when_the_new_start_comes_first` (Start -> поздний End: B занимает слот A по pid, тема переименована, иконка «нет канала» не появляется; после разрыва линка переподключение со старым env id снова даёт «alive» на теме B, то есть поздний End не отнял pid у B).
10. `crates/cctg/tests/agent_stdio.rs` (новый): настоящий бинарник, pipes, `USERPROFILE`/`HOME` во временный каталог, чистый env. Скрипт из 11 строк: initialize, initialized, `ping`(6), tools/list(2), reply(3), неизвестный метод(4), мусор, пустая строка, запрос без `jsonrpc`(7), запрос со скалярными `params`(8), tools/list(5). Ровно 9 строк stdout, каждая JSON-объект с `jsonrpc: "2.0"`: `ping` -> `{}`, id 4 -> `-32601`, id 8 -> `-32600`, есть `-32700` и `-32600` с `id: null`. Hub, который принимает и сразу рвёт соединение, гарантирует реальный warn в stderr во время сеанса; в stdout нет `hub link`, `WARN`, `INFO`, секрета; в stderr нет секрета; закрытие stdin завершает процесс с кодом 0 быстрее 5 с. Второй тест: `sdk-cli`, без секрета, без session id -> те же 9 ответов, `reply` с `isError`, подключений нет.
11. `crates/cctg/tests/ingress_logs.rs`, `tests/slots_logs.rs`: только `claude_pid: None` в литералах.

### Шаг 3. Не делать

- Не менять `wire::VERSION`, `hub/ingress.rs` (кроме литерала в тесте), планировщик, Bot API, рендер Telegram.
- Не добавлять маршрутизацию тема <-> агент (TASK-021), кнопки permission (TASK-014), стриминг (TASK-016), headless-транспорт (TASK-019), флаг или эвристику «канал подтверждён» (решение orchestrator).
- Не коммитить `target/`, `.cctg/`, `.env`. Сообщения коммитов без "Generated with" и "Co-Authored-By".

## 3. Test plan

### Автоматические проверки (обязательны)

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task013-target'
$env:CARGO_PROFILE_DEV_DEBUG = '0'   # только если без него LNK1104/D8050
cargo test --workspace --offline -j 1
cargo clippy -p cctg --all-targets --offline -j 1 -- -D warnings
cargo fmt --all -- --check
git diff --check
git diff --stat   # ровно 13 файлов из hashes.txt
```

Ожидается: 311 passed, 0 failed, 1 ignored (так на `REF`: `T/scratch/reviewer2/cargo_test.txt`); clippy и fmt чисты (`clippy.txt`). Число passed может вырасти, если исполнитель добавит тесты; падений быть не должно, ignored остаётся 1.

### Покрытие критериев

| Критерий | Чем закрыт |
|---|---|
| initialize -> initialized -> tools/list -> tools/call, по одному JSON-объекту на строку | `channel::tests::{initialize_declares_the_channel, tools_list_offers_reply_only, reply_goes_to_the_hub}`, `agent_stdio::a_full_session_with_a_failing_hub_keeps_stdout_pure` |
| неизвестный метод `-32601`, битый ввод контролируем | `channel::tests::{bad_calls_are_answered_not_fatal, broken_input_gets_controlled_answers, only_json_rpc_2_0_with_structured_params_is_served, a_notification_without_json_rpc_2_0_does_not_initialize}`, `agent::tests::frames_are_lines_and_an_oversized_line_is_skipped_whole`, `agent_stdio` |
| `ping` и версия протокола (MCP) | `channel::tests::{ping_answers_an_empty_result_at_any_time, initialize_negotiates_the_protocol_version}`, `agent_stdio` (ping -> `{}`) |
| stdout без логов и паник, в том числе при недоступном hub | `agent_stdio::a_full_session_with_a_failing_hub_keeps_stdout_pure` (warn реально пишется, урок TASK-002), panic hook, `tests/stdout.rs` |
| невалидный ключ meta отброшен, валидные байт в байт | `channel::tests::{inbound_waits_for_initialized_and_keeps_valid_meta_byte_for_byte, meta_keys}`, `agent::tests::the_channel_relays_both_ways_and_survives_a_hub_restart` |
| разрыв и восстановление hub, та же сессия регистрируется снова | `agent::tests::{the_channel_relays_both_ways_and_survives_a_hub_restart, the_agent_reconnects_and_registers_again_after_a_hub_restart}` |
| permission relay: один раз на открытый id | `channel::tests::{permission_relay_round_trip, a_duplicate_permission_request_is_relayed_once_and_closed_once, malformed_permission_requests_are_not_relayed}` |
| ручной запуск: баннер и inbound; вложенный запуск не регистрируется | живой прогон `T/scratch/live_hub.jsonl` + `debug_LIVE.log` (регистрация, inbound A и B, reply, relay двух permission, вложенный `sdk-cli` без второго соединения); `agent_stdio::headless_and_unconfigured_agents_answer_without_a_hub`; `registry::tests::the_agent_of_a_nested_run_is_never_bound` |
| cwd и хост тем же хелпером, что у хука | `run_stdio` использует `DeviceConfig.host` и `device::canonical_cwd`; живой прогон: `host: live13`, cwd канонический |
| `/clear` проверен наблюдением, перерегистрация | `T/scratch/clear_evidence.txt` (сервер не перезапускается, env id старый); `slots::tests::the_agent_follows_its_claude_process_through_clear`, `..._when_the_new_start_comes_first`, `registry::tests::the_pid_of_a_cleared_process_names_the_new_session` |
| команда установки с абсолютным путём | `agent::tests::install_command_quotes_the_absolute_path`, разбор `agent-install` в `main` |
| Existing tests pass | весь workspace |

### Доказательства на `REF` (уже сделано, повторять не обязательно)

- Воспроизведение до исправления: `T/scratch/reviewer2/repro_before_fix.txt`, 5 новых тестов падают на коде планировщика (ping -> `Null` вместо `{}`, эхо `9999-99-99`, id 1 вместо `null` без `jsonrpc`, `initialized` без `jsonrpc` исполнен, дубликат permission переслан).
- Исправления: `T/scratch/reviewer2/fix.py` (только исходник `channel.rs` и `agent_stdio.rs`); тест порядка `/clear`: `add_clear_order_test.py` добавил первую версию, окончательный текст теста только в `REF/crates/cctg/src/hub/slots.rs` (и в патче).
- Мутации (`mutate.py`, `mutations.out.txt`, `mutations_clear_order.out.txt`): все 15 убиты. Пять мутаций планировщика повторены на исправленном коде (M1 без `follow_pid`, M2 без pid-fallback, M3 без пропуска `sdk-cli`, M4 meta без фильтра, M5 привязка к вложенной сессии). Новые: N1 без `ping`, N2 без проверки `jsonrpc`, N3 эхо id при не-2.0, N4 скалярные `params` приняты, N5 эхо неизвестной версии, N6 fallback при отсутствии версии, N7 дубликат permission переслан; C1-C3 для обратного порядка `/clear` (без `follow_pid`, без pid-fallback, поздний `SessionEnd` чистит pid новой сессии).
- Стабильность: 5 прогонов подряд `slots::tests::the_agent_follows*` и `agent_stdio` без падений (`flake.out.txt`).

### Живые проверки (для QA, по желанию; исполнителю не нужны)

Только временный `--mcp-config`, синтетический секрет, копия бинарника под `%TEMP%`, `T/scratch/planner/fake_hub.py`; пользовательский конфиг не трогать. Рецепт: PLAN.md, раздел 4, последний абзац (`fake_hub.py <port> <secret> <log> --inbound ...`, `run_interactive_scenario.py -- --mcp-config <mcp.json> --dangerously-load-development-channels server:cctg --model haiku --debug-file <log>`).

1. С флагом: debug-строка `Channel notifications registered` (баннер на 2.1.280 свёрнут), inbound доходит, `reply` приходит в mock hub, permission request/verdict проходят.
2. `/clear`: один pid MCP-сервера, старый env id, новый `SessionStart(source=clear)` переносит привязку на hub, агент не перезапускается.
3. Вложенный `claude -p`: `sdk-cli`, MCP отвечает, второго соединения с hub нет.
4. Без флага: повторно не гонять. TASK-004 уже доказал: сервер спавнится как обычный MCP, inbound Claude Code молча выбрасывает. Агент этого не видит и регистрируется как обычно (см. rollout).

После прогона удалить временный каталог и его папку в `~/.claude/projects`. Ключи `projects[...]` в `~/.claude.json`, созданные самим Claude Code, чистит пользователь.

## 4. Rollout notes

- Миграций нет. `registry.json` и `wire::VERSION` не меняются. `Register.claude_pid` необязателен: старый агент без поля декодируется в `None` (тест), новый hub со старым агентом работает без переноса по `/clear`.
- Новая переменная устройства `CCTG_HUB_AGENT_ADDR` (env или `~/.cctg/device.env`), по умолчанию `127.0.0.1:47291` (совпадает с `CCTG_AGENT_LISTEN` hub по умолчанию). Нужен `CCTG_HUB_SECRET`, как у хука; без него агент работает в режиме «нет hub» (одна warn-строка в stderr, `reply` возвращает `isError`).
- Установка это шаг пользователя: `cctg agent-install` печатает `claude mcp add --scope user cctg -- "<abs exe>" agent`; пользователь выполняет её сам и запускает Claude Code с `--dangerously-load-development-channels server:cctg`. Сервер user scope спавнится в каждой сессии на машине; без флага он работает тихо.
- Режим «без флага» агенту неотличим: `initialize` и `tools/*` выглядят одинаково, у inbound нет ack. Агент регистрируется, сессия в hub выглядит «alive», хотя Claude Code inbound выбросит. Флага «канал подтверждён» нет по решению orchestrator (OPEN_DECISIONS.md); буфер на стороне hub остаётся обязательным (TASK-017).
- Любой `claude -p` (вложенный и headless resume от hub) это `sdk-cli`: MCP отвечает, hub-link не создаётся. TASK-019 использует одноразовый prompt/stream-json.
- `claude_pid` не определяется для npm-установки (claude как `node.exe`) и при оборванной цепочке процессов: тогда после `/clear` сессия остаётся «нет канала» до перезапуска Claude Code.
- При `/clear` в порядке End -> Start иконка темы может на мгновение стать «dead» (поведение TASK-011).
- Маршрутизации Telegram <-> агент после этой задачи нет: транспорт доказан на mock hub, E2E через Telegram появится в TASK-021.
- После merge: в `maw/project-context/domains/channel.md` строку "Every other method answers method-not-found" дополнить исключением для `ping` и правилами envelope/версии (предложение в `T/PCTX_PROPOSALS.md`, решение OPEN_DECISIONS п. 3).

## 5. Review notes

Disconfirmation (до оценки): самый конкретный контрпример к PLAN_V2 это его правило версии «на неизвестную версию всегда отвечать 2025-11-25». Если клиент старее и просит `2025-06-18`, ответ `2025-11-25` рвёт соединение. Проверено по исходнику MCP TypeScript SDK: `packages/client/src/client/client.ts` бросает `Server's protocol version is not supported` на ревизию вне своего `SUPPORTED_PROTOCOL_VERSIONS` (`packages/core/src/constants.ts`: `[LATEST, '2025-06-18', '2025-03-26', '2024-11-05', '2024-10-07']`), а сервер SDK эхоит любую поддерживаемую. Контрпример подтвердился: версия согласуется по списку, а не одной константой. Claude Code 2.1.280 шлёт `2025-11-25` (`probe_log_clear.jsonl`), для него ответ тот же, поэтому живые доказательства остаются в силе.

Что изменено относительно PLAN_V2:

1. PLAN_V2 предлагал исполнителю применить старый патч и чинить `channel.rs` руками по словесному описанию. Теперь исправления сделаны, собраны и протестированы в `REF`, а патч и хэши пересобраны: исполнитель ничего не пишет сам. Изменились только `channel.rs`, `hub/slots.rs`, `tests/agent_stdio.rs`, остальные 10 хэшей совпадают с патчем планировщика.
2. Все четыре дефекта, найденные reviewer-1, подтверждены падающими тестами (`repro_before_fix.txt`) и исправлены: `ping` -> `{}`; строгий `jsonrpc: "2.0"` и структурные `params`; `initialize` без версии -> `-32602`, версия без эха неизвестной; повторный открытый `request_id` не пересылается, verdict закрывает его один раз.
3. Версия: вместо одной `2025-11-25` поддерживается набор из четырёх опубликованных ревизий (см. disconfirmation). Всё, что использует агент (tools, experimental capabilities, instructions, `isError`), одинаково во всех четырёх.
4. `id` у `-32600`: PLAN_V2 требовал `null` для любого плохого envelope. Оставлено `null`, когда `jsonrpc` не `"2.0"` (это не сообщение JSON-RPC 2.0, id ему не доверяем). Для запроса 2.0 со скалярными `params` id эхоится, чтобы клиент не ждал таймаута; спецификация JSON-RPC требует `null` только когда id не удалось определить. Прежнее поведение для `method` не строкой (эхо id) не менялось.
5. PLAN_V2 требовал проверить `/clear` в порядке Start -> поздний End, но теста не было. Добавлен `the_agent_follows_its_claude_process_when_the_new_start_comes_first`. Он показал, что при обратном порядке B всё равно занимает слот A (тот же pid), и что поздний `SessionEnd` не отнимает pid у B (охрана в `registry.rs`, мутация C3 убита).
6. Процессный тест `agent-install` с синтетическим секретом из PLAN_V2 не добавлен: команда не читает конфиг и секрет вообще (`main.rs`: только `current_exe`), а reviewer-1 уже проверил вывод процесса (`T/scratch/reviewer1/install_stdout.txt`). Unit-тест формата и тест разбора подкоманды остаются.
7. Живой negative-run без флага из PLAN_V2 не делается: поведение уже проверено в TASK-004 (закон домена), а агент в этом режиме по построению ведёт себя так же, как с флагом. Режим записан в rollout как неотличимый.
8. Ожидаемое число тестов обновлено: 311 passed / 0 failed / 1 ignored (305 у планировщика, плюс 5 тестов протокола и 1 тест `/clear`).

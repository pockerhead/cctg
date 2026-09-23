# PLAN — TASK-010: transport contracts, agent TCP и hook HTTP ingress

Stage: planner (claude/opus, effort=medium). Пути даны от корня репозитория `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-010`. `REF` = `T/scratch/planner/ws`: копия workspace (HEAD `b417b00` + 9 файлов), в которой этот план уже реализован и проверен:

- `cargo fmt --all -- --check` чисто, `cargo clippy --workspace --all-targets --offline -- -D warnings` чисто;
- `cargo test --workspace --offline`: 179 passed, 1 ignored (36 новых тестов, то есть на HEAD 143 + 1), вывод в `T/scratch/planner/workspace_test.txt`;
- 10 мутаций, все убиты (`T/scratch/planner/mutations.out.txt`, скрипт `mutate.py`);
- флейки: lib-тесты `wire/agent/hook/hub::ingress/hub::config` 30 прогонов, `tests/ingress_logs.rs` 20 прогонов, 0 падений (`T/scratch/planner/flake.out.txt`);
- `Cargo.lock` не меняется: `mio`/`socket2` для `tokio/net` уже в lock через `reqwest`.

Telegram не вызывался, `.env` не читался.

## 1. Understanding

Что есть сейчас (HEAD):

- `crates/cctg/src/main.rs:13-37`: подкоманды `hub`, `agent`, `hook`. `agent` и `hook` пока пустые (`Command::Agent | Command::Hook { .. } => {}`), `tests/stdout.rs:4-21` проверяет, что они не пишут в stdout. Этот файл задача не трогает.
- `crates/cctg/src/lib.rs`: только `pub mod hub;`.
- `crates/cctg/src/hub/config.rs`: `Config::load`/`from_vars` (строки 88-167). Секреты оборачиваются в newtype с редактирующим `Debug` (`BotToken`, строки 38-52). Опциональные значения, без которых hub не стартует, живут в `Option` и проверяются в `run()` (`projects_dir`, строки 82-84 и `mod.rs:67-70`). По этому образцу сделан `hub_secret`.
- `crates/cctg/src/hub/mod.rs:66-110` `run()`: config → offsets → `BotApi` → getMe/getChatMember → Scheduler → commands worker → `updates::poll`. Никаких сокетов нет.
- `crates/cctg/Cargo.toml:14`: tokio только с `sync`, `time` (плюс `macros`, `rt-multi-thread` из workspace). Для TCP нужны `net` и `io-util`.
- Тесты, которые ловят логи, живут в отдельных бинарниках с глобальным подписчиком и `.without_time()` (`tests/command_logs.rs`, `tests/routing_logs.rs`; риск-уроки TASK-008 в домене hub).
- Реальные payload хуков (`maw/tasks/done/TASK-003/scratch/capture_*.jsonl`): у `SessionStart` ключи `cwd, hook_event_name, model, scratchpad_dir, session_id, source, transcript_path`, у `Stop` `session_id, prompt_id, last_assistant_message, ...`. Идентичности события нет, поэтому ключ дедупа это `event_id`, который чеканит сам хук (Resolved question в `task.md`).
- Доки хуков (https://code.claude.com/docs/en/hooks, проверено 2026-09-23): `SessionEnd` несёт `reason` (`clear|resume|logout|prompt_input_exit|other`), хуки `SessionEnd` делят бюджет 1.5 с. `UserPromptSubmit` и `SubagentStart` несут `prompt_id`, `SubagentStart` ещё `agent_id`, `agent_type`.

Соседние задачи, для которых нужны узкие точки входа: TASK-011 (реестр слотов, потребитель `AgentEvent` и `HookPost`), TASK-012 (`cctg hook`, заполняет `HookEvent` и зовёт `hook::post`), TASK-013 (`cctg agent`, зовёт `agent::spawn`), TASK-014 (permission relay по `PermissionRequest`/`PermissionVerdict`).

## 2. Approach

Четыре модуля в крейте `cctg`. Отдельного `proto`-крейта нет.

1. **`src/wire.rs`, контракты.** `VERSION = 1`. Каждая строка agent-link это один JSON-объект с `"v"` и `"type"` (serde internally tagged). Агент шлёт `AgentMsg`: `hello{secret}`, `register{session_id,host,cwd}`, `reply{text}`, `permission_request{request_id,tool_name,description,input_preview}`. Hub шлёт `HubMsg`: `registered`, `rejected{reason: auth|version|protocol}`, `inbound{content,meta}`, `permission_verdict{request_id,behavior: allow|deny}`. `decode` сначала парсит `serde_json::Value` (длина уже ограничена), потом проверяет `v`, потом `type` по `Kinds::KINDS`, потом поля. Так неизвестная версия, неизвестный тип и битые поля дают три разных `WireError` без паники. `WireError` не несёт входных данных: тексты ошибок `serde_json` цитируют значение (`invalid type: string "..."`), а там может быть секрет. `read_line` читает через `take(MAX_LINE)` (1 MiB), поэтому пир без `\n` не может раздуть буфер больше лимита; тест гоняет бесконечный `tokio::io::repeat`. `Secret` сделан newtype с `Debug = <redacted>`, а сравнение в нём constant-time (xor-fold, `black_box`). Наружу утекает только совпадение длины, как в `subtle`/OWASP. Hook-контракт: `HookPost{v,event_id,host,session_id,cwd,transcript_path,event: HookEvent}`, где `HookEvent` это 7 вариантов (6 событий + `subagent_handback`). `EventId` это 32 hex, 128 бит из двух SipHash-хэшей со случайным ключом std `RandomState` (ключ из ОС) по счётчику, часам и pid. Новых крейтов нет. `HookPost::new` чеканит id один раз, повторная отправка того же значения несёт тот же id.
2. **`src/hub/ingress.rs`, hub.** `bind(addr)` предупреждает в лог, если адрес не loopback. `serve_agents(listener, secret, mpsc::Sender<AgentEvent>)` принимает соединения. Первая строка должна быть `hello` с верным секретом, иначе `rejected{auth}`, закрытие, и `register` не читается вовсе. Вторая строка `register`, дальше `AgentEvent::Registered{conn, register, to_agent}`, ответ `registered` и цикл: `reply`/`permission_request` уходят в hub как `AgentEvent::Message`, неизвестные типы и битые строки пропускаются с warn, смена версии или строка сверх лимита закрывают соединение, `Disconnected` шлётся в конце. Задачи соединений живут в `JoinSet`, поэтому остановка `serve_agents` рвёт все соединения; для агента это и есть рестарт hub. `serve_hooks(listener, secret, mpsc::Sender<HookPost>)` это самописный HTTP/1.1: только `POST /v1/hook`, заголовки до 8 KiB, `Authorization: Bearer` проверяется до чтения тела, нужен ровно один `Content-Length` не больше 1 MiB, `Transfer-Encoding` и повтор `Content-Length` дают 400 (RFC 9112 §6.3, защита от request smuggling), общий дедлайн 2 с, `Connection: close`. Тело декодируется `decode_hook`. Дедуп `Dedup` держит до 4096 id не дольше 10 минут. id запоминается только после успешного `try_send` в канал hub; при полном канале ответ 503 без запоминания, и повторная отправка дойдёт. После раннего ответа (401, `rejected`) делается lingering close: shutdown записи и слив до 64 KiB за 250 мс, чтобы клиент получил ответ, а не RST.
3. **`src/agent.rs`, агент.** `spawn(LinkConfig) -> (Sender<AgentMsg>, Receiver<LinkEvent>)`. Задача подключается (таймаут 5 с), шлёт `hello`+`register`, ждёт `registered`, отдаёт `LinkEvent::Up`, потом гоняет строки в обе стороны. При потере связи отдаёт `Down`, спит `Backoff::delay(attempt)` и подключается снова с тем же `register`. Backoff экспоненциальный с "equal jitter" (задержка равномерно в `[d/2, d]`, `d = min(max, initial·2^n)`, по умолчанию 250 мс..30 с), схема из AWS Architecture Blog "Exponential Backoff and Jitter". Equal, а не full jitter: full может выродиться в почти нулевые паузы (aws-sdk-net #4341). Реконнект есть только у агента.
4. **`src/hook.rs`, хук.** `post(addr, secret, &HookPost, timeout)` это один POST голым `TcpStream`, одним общим таймаутом на всё (connect, запись, чтение статуса), без цикла повторов. `Ok` только на 204. `reqwest` не берём: у него старт клиента с rustls, а бюджет `SessionEnd` 1.5 с общий для всех хуков.

Hub `run()` берёт `CCTG_HUB_SECRET` (обязателен), слушает `CCTG_AGENT_LISTEN` (по умолчанию `127.0.0.1:47291`) и `CCTG_HOOK_LISTEN` (по умолчанию `127.0.0.1:47292`). bind идёт до обращений к Telegram, занятый порт сразу даёт понятную ошибку. Пока TASK-011 не подключил реестр, события уходят во временный `drain_ingress`, который только логирует. Не-loopback адрес считается явной настройкой: host-имена не резолвятся, принимается только `ip:port`, и при старте пишется warn.

Почему не крейт HTTP-сервера: эндпоинт один, формат фиксирован, клиент тоже наш. Самописный разборщик на ~100 строк закрывает все опасные места (ограниченные заголовки и тело, дедлайн, отказ от chunked/TE, единственный Content-Length) и покрыт тестом на каждую ветку. hyper/axum не входят в согласованный набор крейтов и тянут поверхность, которая здесь не нужна. Решения записаны в `T/log.jsonl`.

## 3. Steps

### Step 0. Изоляция и baseline

1. `git status --short -- Cargo.toml Cargo.lock crates` пуст. Если нет, остановиться.
2. `.env` не открывать, в Telegram не ходить, `~/.claude` не трогать.
3. Cargo запускать по одной команде за раз (памяти мало), target вне репозитория и свой:
   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --offline -- -D warnings
   cargo test --workspace --offline
   ```
   Ожидание на HEAD: всё зелёное, 143 passed + 1 ignored (179 минус 36 новых тестов этого плана).

### Step 1. Скопировать 9 файлов из REF байт в байт

Источник `T/scratch/planner/ws/<path>`, назначение `<path>` в корне репозитория. Файлы в REF в UTF-8 без BOM, с LF (при `core.autocrlf=true` git сам нормализует; хэши считаются по скопированным байтам, до любого git checkout). Руками не править. Проверка из корня репозитория:

```bash
sha256sum -c maw/tasks/in_progress/TASK-010/scratch/planner/hashes.txt
```

Все 9 строк `OK`. Полный diff против HEAD: `T/scratch/planner/final.diff`.

| Path | Вид | SHA-256 |
|---|---|---|
| `crates/cctg/Cargo.toml` | edit: tokio features `["io-util", "net", "sync", "time"]` | `32830ff594d896dee6c2b2349593f07ff67497555d40d88519bab08cfb7c244c` |
| `crates/cctg/src/lib.rs` | edit: `pub mod agent; pub mod hook; pub mod wire;` | `94eb3c715fdb64ba6f839a8e829c276174d1ea9372d302cfe1d74fd4917e0130` |
| `crates/cctg/src/wire.rs` | new | `9d1eca6e53dabccac44ba3261da31cbeed65e96a6701b614d4128fa89f59279b` |
| `crates/cctg/src/agent.rs` | new | `6fea93e747d06bc5242363d90832ddda199bd582aacffb12afef1b330a78c6a7` |
| `crates/cctg/src/hook.rs` | new | `7a7c39bb1b23019bac58c07e4fb3d2f8879a2d2b798499b62d01fb02db1211d8` |
| `crates/cctg/src/hub/mod.rs` | edit | `ee1986535f446d36caed5ee506d27a375e50779ea555d6ddf8ffec1f368192ab` |
| `crates/cctg/src/hub/config.rs` | edit | `7e70841fca6a7e09b8e32b3cf924db24ede6061bc39b6973bec0ff96064ff05f` |
| `crates/cctg/src/hub/ingress.rs` | new | `7f4142a064b70225c40ee2922e3866cbbff46bed731a33b5a05e625bd54c2437` |
| `crates/cctg/tests/ingress_logs.rs` | new (отдельный тест-бинарник) | `a7403f6798db87b767dfd5740371f963b263d2432772103e4cee8a939e440c9e` |

`Cargo.lock`, `main.rs` и остальные файлы не меняются.

Что внутри каждого файла и какой критерий приёмки он закрывает:

**1a. `wire.rs`** (контракты, критерии 1, 5, 7).
- Константы: `VERSION=1`, `MAX_LINE=1 MiB`, `MAX_HOOK_BODY=1 MiB`, `HOOK_PATH="/v1/hook"`, `MIN_SECRET_LEN=16`.
- `WireError {TooLong, Malformed, Version, UnknownKind, Closed, Io(ErrorKind)}`, все тексты фиксированные.
- `Secret::parse` (trim, только `is_ascii_graphic`, не короче 16), `expose`, `matches` (constant-time), `PartialEq` через `matches`, `Debug` редактирует. `Deserialize` у `Secret` без валидации: сравнивается то, что прислали.
- `Kinds` и `AgentMsg`/`HubMsg`/`Register`/`PermissionRequest`/`Rejection`/`Behavior`, как в разделе 2. Поля, которые допишут позже, добавлять с `#[serde(default)]` без смены версии; новый тип сообщения меняет версию (правило в doc-комментарии модуля).
- `encode` (flatten в `{v, ...}` + `\n`), `decode`, `read_line`, `write_msg`.
- `EventId` (`try_from = String`, ровно 32 hex в нижнем регистре), `random_u64` (`pub(crate)`, его же использует jitter агента).
- `HookPost::new`, `HookEvent` (7 вариантов, `kind()`), `decode_hook`.
- Тесты: round-trip всех вариантов обоих направлений, одна строка с одним `\n` и `v=1`; `KINDS` совпадают с enum; version 2, отсутствующий или строковый `v`, неизвестный тип, битые поля, не-JSON, пустая строка; неизвестные поля игнорируются; ошибка с секретом внутри не показывает его ни в `Display`, ни в `Debug`; правила секрета; `read_line` на бесконечном `repeat` даёт `TooLong` при `capacity <= 2*MAX_LINE`; строка ровно в лимит проходит; EOF посреди строки даёт `Closed`; round-trip всех `HookEvent`; ошибки `decode_hook`; 10 000 разных `EventId`.

**1b. `hub/ingress.rs`** (критерии 2, 4, 5, 6, 7).
- `bind`, `AgentEvent`, `serve_agents`, `agent_session`, `reject` + `linger`, `short` (8 символов session id для логов).
- `Dedup{new, contains, insert, len, is_empty}`, константы `DEDUP_MAX=4096`, `DEDUP_TTL=10 min`, время передаётся параметром `now` (тестируется без часов).
- `Status` (204/400/401/404/405/411/413/431/503), `serve_hooks`, `hook_request`, `accept_hook`, `read_request`.
- Логи: только `conn`, `peer`, `session` (8 символов), тип события, код статуса, фиксированный текст. Путей, `cwd`, `host`, текстов и секрета в логах нет.
- Тесты: неверный секрет и `register` без `hello` отвергаются до `Register`, событий нет; `v=2` в `hello` даёт `rejected{version}`; зарегистрированный агент обменивается сообщениями в обе стороны, мусор между ними пропускается, отключение даёт `Disconnected`; поток без `\n` больше `MAX_LINE` рвёт соединение; bind на loopback и на `0.0.0.0` (подключение через 127.0.0.1); повтор POST доставляется один раз, а новый `SessionStart` той же сессии (resume) и два `Stop` одного `prompt_id` доставляются оба; каждая ветка ошибок HTTP; 503 при полном канале и успешная повторная отправка после него; тело по 7 байт за запись; `Dedup` по TTL и по размеру.

**1c. `agent.rs`** (критерий 3).
- `Backoff{initial,max}` + `Default` + `delay`, `LinkConfig{addr: String, secret, register, backoff}`, `LinkEvent{Up, Down, Message}`, `spawn`, внутренние `run`/`connect`/`serve`. Повторяющийся warn про одну и ту же ошибку подключения не спамит: дальше идёт debug.
- Тесты: `delay` всегда в `[d/2, d]` и есть разброс. Главный тест рестарта: hub на порту P, `Up`, `Registered` в hub, сообщение hub→агент дошло; hub остановлен (`abort` задачи `serve_agents`), агент отдаёт `Down`; на P встаёт слушатель, который принимает и сразу закрывает, пять попыток идут с растущими паузами (все ≥ 5 мс, пятая ≥ 60 мс, то есть не tight loop); потом на P снова настоящий hub, агент отдаёт `Up`, hub видит повторный `Register` с тем же `session_id`, а `reply`, поставленный в очередь во время простоя, доходит. Ещё: отвергнутый секрет не даёт `Up`; drop получателя останавливает link, hub видит `Disconnected`.

**1d. `hook.rs`** (критерии 4, 5).
- `PostError{Timeout, Io(ErrorKind), Status(u16), BadResponse}` без секрета в текстах, `post`, `parse_status`.
- Тесты: два `post` одного `HookPost` дают одно событие в hub, первый укладывается в 500 мс; неверный секрет даёт `Status(401)`; молчащий сервер возвращает `Timeout` не позже чем через 1 с при таймауте 300 мс; закрытый порт даёт ошибку меньше чем за 1.5 с; `Display`/`Debug` ошибки не содержат секрет.

**1e. `hub/config.rs`** (критерии 6, 7).
- Константы `SECRET_VAR="CCTG_HUB_SECRET"`, `AGENT_LISTEN_VAR="CCTG_AGENT_LISTEN"`, `HOOK_LISTEN_VAR="CCTG_HOOK_LISTEN"`, `DEFAULT_AGENT_LISTEN=127.0.0.1:47291`, `DEFAULT_HOOK_LISTEN=127.0.0.1:47292`.
- `ConfigError::Secret(SecretError)` и `ConfigError::ListenAddr(&'static str)`; значение в текст ошибки не попадает.
- Поля `Config.hub_secret: Option<Secret>`, `agent_listen`, `hook_listen`.
- Существующий тест `debug_hides_token_and_user_ids` переименован в `debug_hides_token_user_ids_and_hub_secret` и дополнительно проверяет секрет. Новые тесты: `listeners_default_to_loopback`, `non_loopback_listeners_need_an_explicit_address` (Tailscale IPv4, `[::]`, отказ для `localhost:5000`, адреса без порта, порта вне диапазона), `a_bad_hub_secret_is_named_but_not_echoed`. Остальные тесты конфигурации не меняются: секрет в `from_vars` опционален.

**1f. `hub/mod.rs`.** `pub mod ingress;`. В `run()` после offsets: секрет через `with_context` (текст называет переменную), два `ingress::bind` с контекстом, указывающим на переменную. После запуска commands worker: два bounded-канала по 256, `serve_agents`, `serve_hooks`, `drain_ingress`. `drain_ingress` это временный потребитель до TASK-011; он логирует только `conn` для `AgentEvent::Message` и отбрасывает остальное.

**1g. `tests/ingress_logs.rs`** (критерии 2, 7). Отдельный бинарник, глобальный подписчик, `.without_time()`, TRACE. Прогоняются все пути: неверный секрет, `hello` с секретом в поле неверного типа, `v=9` с секретом, строка сверх лимита, начинающаяся с секрета, у зарегистрированного агента неизвестный тип с секретом в имени, поле неверного типа рядом с секретом, не-JSON с секретом, `reply` с контентом; для HTTP неверный bearer, битое тело с секретом, неизвестный тип события с секретом, хороший POST и его повтор. Проверки: в логах есть все 6 ожидаемых строк (тест не пустой), нет ни настоящего секрета, ни неверного секрета, ни маркера контента (он же в `host`/`cwd`/`transcript_path`).

### Step 2. Проверка

По одной команде:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание: 179 passed, 1 ignored. Прогонять весь workspace по кругу не нужно. Для флейков достаточно `cargo test -p cctg --offline --lib -- wire:: agent:: hook:: hub::ingress::` 10 раз и `--test ingress_logs` 5 раз.

### Step 3. Пользовательский шаг (не для implementer)

После мержа `cctg hub` не стартует без `CCTG_HUB_SECRET` в `.env` (16+ видимых ASCII, например `openssl rand -hex 32`). Implementer `.env` не трогает. В заметках задачи и в сообщении пользователю это надо написать явно.

## 4. Risk areas

- **Живой hub после мержа** потребует новый секрет и два свободных порта 47291/47292. Если порт занят, hub падает на старте с понятной ошибкой. Порты можно переопределить.
- **Windows и RST.** Закрытие сокета с непрочитанным вводом шлёт reset, и ответ клиенту может потеряться. Помогает lingering close. На этом хосте 30+20 прогонов прошли без падений, но это эвристика: 64 KiB и 250 мс. Хук с телом больше 64 KiB и неверным секретом может получить reset вместо 401. Для хука это одно и то же: ошибка, exit 0.
- **Повторный bind того же порта** в тесте рестарта: `rebind` пробует 100 раз по 50 мс. На Windows сразу после закрытия bind на тот же порт иногда отказывает; на этом хосте прошло сразу.
- **Потеря сообщения на разрыве.** Сообщение, запись которого упала, теряется (это написано в doc-комментарии `agent.rs`). Для `permission_request` это значит, что запрос не дойдёт до Telegram, но терминальный диалог Claude Code остаётся открытым. Повторную отправку после `Up` решает TASK-013/014, если понадобится.
- **Полуоткрытое TCP** (удалённое устройство уснуло): без keepalive hub узнает о разрыве только при записи, агент только при чтении. Для loopback это неважно. Для Tailscale (шаг 5 плана) это открытый вопрос, ниже.
- **Дедуп по окну.** Больше 4096 событий за 10 минут вытесняют старые id, и поздний повтор такого события пройдёт. Для одного хоста это нереальный поток. Hub рестартует с пустым окном, и повтор, пришедший через рестарт, тоже пройдёт.
- **`EventId` не криптографический.** Нужна уникальность, а не секретность, запрос и так аутентифицирован. Коллизия 128 бит практически невозможна.
- **Контракт `HookEvent` предварительный** в части `claude_pid`/`parent_claude_pid`: TASK-012 может поменять эти поля. v1 ещё нигде не развёрнут, поэтому до TASK-018 менять можно без смены версии; после этого только через `#[serde(default)]`.
- **Секрет в памяти** лежит в `String` без zeroize. Крейта нет в наборе; модель угроз локальная.

## 5. Open questions

1. Откуда `cctg agent` и `cctg hook` берут адрес hub и секрет. Они стартуют в папке проекта, `./.env` там чужой. Варианты: env `CCTG_HUB_SECRET`/`CCTG_HUB_ADDR` из регистрации в `~/.claude.json`/settings (попадёт в конфиг Claude Code), или общий файл `~/.cctg/.env`. Решать в TASK-012/013. Эта задача даёт только библиотечные функции с параметрами `addr` и `secret`.
2. TCP keepalive или ping/pong для удалённых агентов (шаг 5): у `tokio::net::TcpStream` нет keepalive без `socket2`. Предлагается решать в задаче про второе устройство, не здесь.
3. Нужна ли хуку хотя бы одна повторная отправка (например, на `ConnectionRefused` сразу после рестарта hub). Контракт её поддерживает (тот же `HookPost`, тот же `event_id`), но цикла повторов в хуке нет по условию задачи. Решает TASK-012 в рамках бюджета 1.5 с.

Источники: RFC 9112 §6.3 (https://www.rfc-editor.org/rfc/rfc9112#section-6.3), AWS Architecture Blog "Exponential Backoff And Jitter" (https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/), aws/aws-sdk-net #4341 (full jitter и почти нулевые паузы), Claude Code hooks reference (https://code.claude.com/docs/en/hooks).

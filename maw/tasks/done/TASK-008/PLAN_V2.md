# PLAN V2 — TASK-008: hub — Telegram Bot API client and outbound scheduler

Stage: plan-reviewer-1 (codex/gpt-5.6-sol, effort=medium). Все пути ниже относительны корня репозитория `C:/Users/user/dev/cctg`. `T` = `maw/tasks/in_progress/TASK-008`.

## 1. Review notes

### Что в исходном плане неверно

1. **Reference нарушает обязательный FIFO внутри темы.** В `scratch/planner/ws/crates/cctg/src/hub/scheduler.rs:72` полоса `Permission` всегда проверяется раньше `Message`, а `pick` на строке 364 выбирает её независимо от `thread_id`. Сценарий «обычное сообщение A в topic 7, затем permission B в topic 7» фактически отправляет `B, A`. Существующий тест `permission_prompt_jumps_the_queue` (`scheduler.rs:545`) использует разные темы 1 и 2 и поэтому скрывает дефект. Добавленный reviewer-тест на одной теме упал; доказательство: `T/scratch/reviewer1/fifo_probe.out.txt`. Контрпример подтвердился.

2. **Ошибка разбора `.env` раскрывает token и allowlist id в stderr.** `Config::load` сохраняет исходную `dotenvy::Error` через `anyhow::Context` (`config.rs:84-90`). При незакрытой кавычке `dotenvy` включает в Display всю оставшуюся часть файла. Локальный probe с фиктивными значениями показал их в CLI stderr; реальный `.env` не читался. Доказательство: `T/scratch/reviewer1/env_error_probe.out.txt`. Следовательно, утверждение исходного плана о безопасности «на любом пути ошибки» неверно, даже при правильном `reqwest::Error::without_url()`.

3. **Побайтово копировать reference нельзя.** Хэши `config.rs` и `scheduler.rs` относятся к двум дефектным версиям выше. После исправлений изменятся также тесты; проверка должна идти по поведению и diff, а не по старым SHA-256.

4. **Количество тестов посчитано неверно.** Независимый offline-прогон reference прошёл, но дал 25 тестов `cctg` и 71 тест `transcript`, всего 96, а не 82. Baseline содержит 2 теста `cctg` и те же 71 тест `transcript`, всего 73, а не 59. Исходный план перечисляет правильные размеры suite, но неверно складывает их. Доказательство независимого прогона: `T/scratch/reviewer1/build_verify.out.txt`.

5. **Указанный HEAD устарел, но исходная кодовая база не изменилась.** Текущий HEAD — `52e9ce6`; между ним и `a2ff7cb` добавлены только артефакты планирования TASK-008. `Cargo.toml`, `crates/cctg/src/main.rs` и `crates/cctg/tests/stdout.rs` по-прежнему соответствуют описанному no-op baseline. Плану не следует требовать конкретный HEAD, достаточно проверить diff реализационных файлов.

6. **Проверка startup rights в reference по существу верна.** `getMe`, затем `getChatMember`, затем `check_topic_rights` выполняются до `Scheduler::new` и `updates::poll` (`hub/mod.rs:48-66`). Тест проверяет `creator`, администратора с/без `can_manage_topics` и не-админские статусы. Эту часть менять не нужно.

### Результат независимой и исследовательской проверки

- В копии `T/scratch/reviewer1/ws` с `CARGO_TARGET_DIR` под `%TEMP%` последовательно прошли `cargo fmt --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings` и `cargo test --workspace --offline`. Реальный Telegram API не вызывался, значения `.env` не читались.
- Официальный [Telegram Bot API](https://core.telegram.org/bots/api) подтверждает envelope `{ok,result,description,error_code,parameters}`, `retry_after`, `getUpdates.offset`, `allowed_updates`, forum service fields, `getChatMember`, `createForumTopic` и `editForumTopic`. Важно: `allowed_updates` не фильтрует уже накопленные апдейты, поэтому raw `Value` + поэлементный tolerant routing обоснованы.
- Официальный [Telegram Bots FAQ](https://core.telegram.org/bots/faq) подтверждает, что offset должен быть `last update_id + 1`, иначе апдейты повторяются.
- Документация [reqwest::Error](https://docs.rs/reqwest/latest/reqwest/struct.Error.html) предупреждает, что ошибка может содержать полный URL, и рекомендует `without_url()`. Reference делает это на каждом пути `reqwest::Error` (`api.rs:39`, send/decode paths); эту часть сохранить.
- Документация [dotenvy::from_path](https://docs.rs/dotenvy/latest/dotenvy/fn.from_path.html) подтверждает нужный precedence: уже существующие process env variables сохраняются. Нельзя лишь протаскивать текст `dotenvy::Error` наружу.
- Документация [reqwest TLS](https://docs.rs/reqwest/latest/reqwest/tls/) подтверждает, что feature `rustls` в reqwest 0.13 использует встроенный provider aws-lc-rs; отмеченный в плане риск C toolchain реален.

## 2. Updated understanding

### Текущее состояние репозитория

- Один workspace с `crates/cctg` и `crates/transcript`; отдельный hub crate не нужен.
- `crates/cctg` сейчас bin-only. `hub`, `agent`, `hook` существуют как subcommands, но все являются no-op.
- В workspace уже есть `anyhow`, `clap`, `serde`, `serde_json`, `tokio`, `tracing`, `tracing-subscriber`; для этой задачи нужны `dotenvy`, `reqwest` и `thiserror`, а также tokio features `sync`, `time` и dev-only `test-util`.
- `.env`, `.cctg/` и `registry.json` gitignored. Новый обязательный `CCTG_ALLOWED_USER_IDS` утверждён оркестратором; пользователь добавит значение перед первым живым запуском.
- `crates/transcript/**` не меняется.

### Подтверждённая форма реализации

`crates/cctg` становится lib+bin package: `src/lib.rs` экспортирует `hub`, а `main.rs` вызывает `cctg::hub::run`. Hub состоит из:

| Файл | Ответственность |
|---|---|
| `crates/cctg/src/hub/config.rs` | `.env`/process-env precedence, валидация token/chat/allowlist, redacted Debug и санитизированные ошибки загрузки файла |
| `crates/cctg/src/hub/api.rs` | узкий bare-`reqwest` Bot API client, 11 методов, envelope и typed operational errors |
| `crates/cctg/src/hub/updates.rs` | tolerant long polling, allowlist gate, service-message classification, offset/backoff |
| `crates/cctg/src/hub/scheduler.rs` | единый actor/outbox, общий message bucket, FIFO по topic, permission priority между topic, edit coalescing и queue-wide 429 pause |
| `crates/cctg/src/hub/mod.rs` | startup config/API/rights check, запуск scheduler и poller |

Reference корректен для API client, update routing, startup check, утверждённой формы bucket и большей части тестовых fake-ов. `config.rs` и алгоритм message queues в `scheduler.rs` требуют исправления до переноса.

Решения из `OPEN_DECISIONS.md` окончательны: переменная `CCTG_ALLOWED_USER_IDS`; bucket capacity 5, refill 1 token/4 s, min gap 1 s; обычные сообщения сохраняют глобальный FIFO между темами в MVP; fairness/round-robin отложен; live-long-poll RSS измеряется позже в TASK-009.

## 3. Revised approach

### Конфигурация и секреты

- `Config::load(Some(path))` читает только заданный файл; без аргумента — только `./.env`, если он существует. Не использовать `dotenvy::dotenv()`, которое ищет файл по родителям.
- Process env имеет приоритет над файлом.
- Добавить вариант ошибки загрузки env-файла, который хранит/показывает только безопасный path и общий текст. Ошибку `dotenvy` **не** помечать `#[source]`, не оборачивать через `anyhow::Context` и не выводить через Debug/Display: parse error содержит исходные строки.
- `BotToken`, `Allowlist`, `Config` и `BotApi` не должны печатать secret или ids. Ошибки bad value называют только имя переменной/номер элемента.
- Все `reqwest::Error` переводятся только через `ApiError::http(error.without_url())`; response body читается bytes и разбирается вручную, без `error_for_status`/`Response::json`.
- Routed payload не содержит `from.id`; logger пишет только enum-reason/service kind/thread id, но не raw update.

### Bot API и polling

- Сохранить узкие `#[serde(default)]` типы только для нужных полей и 11 методов: `getMe`, `getChatMember`, `getUpdates`, `sendMessage`, `editMessageText`, `sendDocument`, `deleteMessage`, `answerCallbackQuery`, `createForumTopic`, `editForumTopic`, `getForumTopicIconStickers`.
- `getUpdates` возвращает `Vec<Value>`. Каждый element независимо преобразуется в `Update`; malformed/unknown update даёт `Ignored`, но следующий valid update обрабатывается, а offset продвигается по любому raw integer `update_id`.
- `allowed_updates = ["message", "callback_query"]`, long poll 50 s, HTTP timeout 65 s. 429 ждёт `retry_after`; прочие polling errors получают exponential backoff 1..30 s и не завершают loop.
- Forum service fields проверяются после chat-id check, но до sender allowlist. Все четыре типа возвращаются как `Routed::Service`, никогда как `Input`; `message_id` и `thread_id` сохраняются для будущего удаления.

### Исправленный scheduler

- Оставить один actor и один request in flight. Все write-операции идут через `Outbox`.
- Metered message traffic (`Send`, `SendDocument`) хранить как **очередь на каждый `Option<thread_id>`**, а каждому job присваивать монотонный enqueue sequence. Голова topic queue — единственный eligible job этой темы; это и есть строгий FIFO.
- Выбор metered job: среди голов topic queues сначала выбрать permission-head с минимальным enqueue sequence; если permission-head нет, выбрать обычную голову с минимальным sequence. Таким образом permission из другой темы может обогнать обычные сообщения, но permission никогда не обгоняет более раннее сообщение своей темы. Для обычного traffic сохраняется принятое MVP-поведение global FIFO, fairness не добавляется.
- `Edit` остаётся отдельной сериализованной очередью с coalescing по `message_id` в исходной позиции; вытесненные waiters получают `Outcome::Superseded`.
- `CreateTopic`, `EditTopic`, `Delete` остаются отдельной сериализованной topic-mutation lane. Ни edit, ни topic mutation не берут message token и не имеют выдуманного численного лимита.
- Общий bucket: capacity 5, refill 1/4 s, min gap 1 s. Неуспешная metered попытка тоже расходует token — это консервативно ограничивает requests, а не только successes.
- Любой `RetryAfter(d)` ставит queue-wide `paused_until`, возвращает job в голову его исходной topic/lane и допускает ровно одну следующую попытку после deadline. Новый 429 снова сдвигает deadline; отдельные retry tasks/timers не создавать.
- Пока bucket ждёт token, actor может выполнять ready unmetered lane; при queue-wide 429 не выполняется ничего. При каждом проходе mailbox следует дренировать ограниченно, чтобы непрерывный producer не отложил dispatch навсегда.

### Startup

- `run`: load config → construct API → `getMe` → `getChatMember(bot_id)` → `check_topic_rights` → optional warning for missing `can_delete_messages` → spawn scheduler → poll.
- `creator` допустим; `administrator` обязан иметь `can_manage_topics`; любой иной status — понятная startup error с `Manage Topics`/`can_manage_topics`. Проверка остаётся до scheduler/poller и до первого `createForumTopic`.

## 4. Revised steps

### Step 0 — baseline и изоляция build

Установить `CARGO_TARGET_DIR` в отдельный каталог под `%TEMP%`; cargo-команды выполнять по одной из-за ограниченной памяти. Не читать `.env`, не запускать real Telegram probes.

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task008-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание для текущего baseline: 73 теста проходят (2 `cctg`, 70 transcript unit/integration, 1 transcript doc-test). Если исходники вне task-artifacts уже изменены, остановиться и сверить diff; не затирать пользовательские изменения.

### Step 1 — manifests и lib/bin wiring

1. В workspace dependencies добавить `dotenvy = "0.15"`, `reqwest = { version = "0.13", default-features = false, features = ["json", "multipart", "rustls"] }`, `thiserror = "2"`.
2. В `crates/cctg/Cargo.toml` подключить workspace `dotenvy`, `reqwest`, `serde`, `serde_json`, `thiserror`; tokio features `sync,time`; dev-only tokio `test-util`.
3. Добавить `src/lib.rs` с `pub mod hub;`.
4. Изменить `hub` subcommand на `Hub { --env-file: Option<PathBuf> }`, вызвать async `cctg::hub::run`; обновить CLI parse tests.
5. Регенерировать/проверить `Cargo.lock` offline. Набор direct dependencies `cctg` должен быть ровно: `anyhow, clap, dotenvy, reqwest, serde, serde_json, thiserror, tokio, tracing, tracing-subscriber`.

### Step 2 — безопасная конфигурация

1. Реализовать `BotToken`, `Allowlist`, `Config`, `ConfigError` и constants `CCTG_BOT_TOKEN`, `CCTG_CHAT_ID`, `CCTG_ALLOWED_USER_IDS`.
2. Валидировать token shape без возврата value, chat id как `-100<digits>`, непустой comma-separated allowlist numeric ids.
3. Реализовать exact-file loading и env precedence.
4. На любой ошибке `dotenvy::from_path` возвращать санитизированный `ConfigError::EnvFile` без source text.
5. Unit tests: complete config, invalid chat form, missing/bad values, redacted Debug.
6. Integration subprocess test: malformed env-file содержит уникальные фиктивные token/id markers; `cctg hub --env-file` завершается non-zero, stdout пуст, stderr называет только файл/общую причину и не содержит ни одного marker или строки переменной.

### Step 3 — Bot API client

1. Перенести узкие structs/envelope и 11 методов из reference, сохранив `#[serde(default)]`.
2. Сохранить ручной `BotApi::Debug`, `ApiError::{Http,RetryAfter,Telegram,Decode}`, `without_url()` и bytes-based decoding.
3. 429 с `parameters.retry_after` маппить в `RetryAfter`; если Telegram прислал 429 без parameters — local fallback 5 s. Не считать этот fallback лимитом topic mutations.
4. Unit tests: successful narrow decode с extra fields; 429 с/без parameters; other JSON/non-JSON/invalid-success responses; реальный loopback transport error во всех Display/Debug/anyhow renderings не содержит runtime token/bot-id marker.

### Step 4 — tolerant inbound routing

1. Реализовать `Routed::{Input,Callback,Service,Ignored}` без sender id в downstream payload.
2. Проверять configured chat; распознавать `forum_topic_created/edited/closed/reopened` до allowlist; затем применять allowlist отдельно к message и callback `from.id`.
3. Реализовать raw batch routing с monotonic next offset и продолжением после malformed/unsupported entries.
4. Реализовать бесконечный long-poll loop с 429 wait и bounded exponential backoff.
5. Tests: allowlisted/stranger messages and callbacks; other chat/no sender; четыре service message типа от bot-like и allowlisted sender; unknown update type, extra fields, wrong field type, missing id/non-object и valid neighbor; captured TRACE logs без всех synthetic sender ids.

### Step 5 — scheduler с реальным per-topic FIFO

1. Реализовать bounded `Outbox`, actor, `Transport` trait/fake и `Job { sequence, op, reply }`.
2. Реализовать per-topic message queues и arbitration из Revised approach; General topic использует key `None`.
3. Реализовать approved token bucket, edit coalescing, unmetered topic/edit lanes и queue-wide 429 pause.
4. Сохранить clean shutdown после drop всех Outbox handles и drain очередей.
5. Обязательные deterministic paused-time tests:
   - 60+ metered attempts под burst по нескольким темам: в каждом полуоткрытом окне 60 s не более 20 attempts, gap не меньше 1 s, последовательность каждой темы сохранена;
   - **обычное A, затем permission B в той же теме → A,B**;
   - обычные сообщения в topic A и permission-head в topic B → permission B может обогнать topic A;
   - цепочка permission/ordinary/permission в одной теме сохраняет enqueue order;
   - повторные edits одного message coalesce, другие message ids сохраняют порядок, waiters получают ожидаемые outcomes;
   - topic mutations/edits не расходуют message tokens; после их burst остаётся полный message burst;
   - 429 ставит на паузу все lanes, retry происходит не раньше deadline и ровно один раз на deadline;
   - несколько последовательных 429 не создают retry tasks/storm;
   - rate-window guarantee сохраняется **после** длинного 429/refill burst, считая и неуспешные attempts;
   - drop Outbox завершает actor после drain.

### Step 6 — startup wiring и CLI behavior

1. Реализовать `RightsError`/`check_topic_rights` и порядок startup до scheduler/poll.
2. Test statuses: creator; administrator with/without relevant rights; member/restricted/left/kicked-or-banned/unknown. Ошибка отсутствующего права содержит `can_manage_topics`.
3. Обновить `tests/stdout.rs`: `agent`/`hook` остаются успешными и молчат в stdout; `hub` без config запускается из пустого temp cwd с удалёнными `CCTG_*`, падает non-zero, stdout пуст, stderr называет только `CCTG_BOT_TOKEN`.

### Step 7 — полная verification и критерии приёмки

Последовательно выполнить:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
cargo tree -p cctg --edges normal --depth 1 --offline
git status --short
```

Проверить соответствие acceptance criteria:

| Критерий | Конкретное доказательство |
|---|---|
| allowlist и отсутствие token/from.id в logs/errors | routing/log capture, runtime reqwest error test, config Debug/error tests, malformed `.env` subprocess test |
| 20/60 и FIFO topic | sliding-window burst test, same-topic permission tests, post-429 rate test |
| edit coalescing и отсутствие retry storm | coalescing waiter test, single/repeated 429 timestamps |
| topic lane не списывает message tokens и не имеет hardcoded rate | mutation/edit burst + full remaining message burst; code review: numeric config используется только message bucket |
| service messages не становятся input | table test всех четырёх `forum_topic_*` |
| unknown fields/types не останавливают polling | mixed raw batch с valid neighbors и advanced offset |
| startup `can_manage_topics` | rights matrix плюс подтверждённый порядок вызовов до spawn/poll |
| measurements записаны | Step 8 |
| existing tests pass | полный workspace test, baseline suites без изменений |

Не использовать старое ожидаемое число 82: после новых regression tests итог будет выше 96. Критерий — прохождение всех перечисленных suite и отсутствие пропавших baseline tests.

### Step 8 — task notes и измерения

Создать `T/IMPL_SUMMARY.md` и перенести уже измеренную таблицу из planner evidence с явным указанием host/rustc/method:

| Metric | HEAD `a2ff7cb` no-op | TASK-008 reference | Evidence |
|---|---:|---:|---|
| Release `cctg.exe` | 994,304 B | 5,485,056 B | planner release artifacts/log |
| Clean release build | 8.5 s / 40 crates | 45.6 s / 127 crates | planner measurement; если исходные build logs отсутствуют в task bundle, так и отметить |
| Idle after TLS/startup scheduler | n/a | working set ≈18.7 MB; private ≈5.3 MB | `T/scratch/planner/rss_probe.out.txt` |

Не запускать `rss_probe`: он читает локальный `.env` и обращается к реальному Telegram. Live-long-poll RSS по решению оркестратора переносится в TASK-009 и не блокирует TASK-008. Не копировать/коммитить probe source, synthetic reviewer inputs или task scratch в product diff.

### Step 9 — final diff

- Product diff ограничить manifests/lock, `crates/cctg/src/{lib.rs,main.rs,hub/**}` и `crates/cctg/tests/stdout.rs`; task note — отдельно.
- `crates/transcript/**`, `.env`, `.claude/`, `registry.json`, project context и чужие изменения не трогать.
- Commit message на английском, без generated/co-author trailers.

## 5. Risk areas

- **Конфликт priority/FIFO.** Permission priority допустим только между головами topic queues. Любой возврат к независимой global Permission lane снова нарушит критерий; same-topic regression test обязателен.
- **Ошибки `.env` как секретный канал.** Даже если собственные `ConfigError` redacted, source от parser раскрывает исходную строку. Нельзя сохранять `dotenvy::Error` в error chain; проверять CLI stderr, а не только unit Display.
- **aws-lc-sys/C toolchain.** `reqwest 0.13 + rustls` собирается на текущем MSVC host, но fresh CI требует CMake/C toolchain. Не переключать provider без отдельного подтверждённого build и обновления task notes.
- **429 и bucket accounting.** Queue-wide pause должен охватывать message/edit/topic lanes. Повторная metered попытка расходует новый token; иначе post-429 burst может нарушить request-window guarantee.
- **Unmetered bursts.** Для edits/topic mutations нет опубликованного численного лимита; их только сериализуют и подчиняют 429. Coalescing ограничивает повторные edits одного message, но distinct-message burst остаётся возможным.
- **Mailbox starvation/backpressure.** Bounded channel защищает память, но бесконечный `while try_recv` может задержать dispatch при непрерывном producer. Дренировать ограниченное число jobs за actor iteration и тестировать finite burst.
- **Pending updates.** `allowed_updates` не влияет на старые pending updates. Raw tolerant routing и advance offset должны пережить неизвестные типы; в этой задаче реальный poll не запускать.
- **Telegram description — внешний текст.** Не логировать raw request/update/body. Human-readable Telegram description допустим в typed API error, но token и `from.id` не должны попадать в request body/error context; если будущий метод добавит user id, его error/log contract потребуется пересмотреть.
- **Anonymous admins.** Сообщение от `GroupAnonymousBot` не проходит user-id allowlist. Это безопасный ожидаемый отказ, но может выглядеть как игнорирование.
- **`message is not modified`.** Сейчас 400 возвращается caller как `Telegram` error; решение считать его success принадлежит будущему caller task, не этому scheduler.
- **Live RSS.** Текущий RSS измерен без активного `getUpdates`; live-long-poll measurement явно отложен в TASK-009.

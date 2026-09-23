# PLAN FINAL — TASK-010: transport contracts, agent TCP и hook HTTP ingress

Stage: plan-reviewer-2 (claude/opus, effort=medium). Пути от корня репозитория `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-010`. `REF` = `T/scratch/reviewer2/ws`: полная копия workspace (HEAD `93b4ede` по коду равен `b417b00`) с этим планом, уже собранная и проверенная:

- `cargo fmt --all -- --check`: чисто. `cargo clippy --workspace --all-targets --offline -- -D warnings`: чисто.
- `cargo test --workspace --offline`: 184 passed, 1 ignored (ignored это старый изолированный config-тест). Лог: `T/scratch/reviewer2/workspace_test.txt`.
- 19 мутаций (10 планировщика + 9 на исправления этого ревью), все убиты: `T/scratch/reviewer2/mutations.out.txt`, `mutations.r4r5.out.txt`, скрипт `mutate.py`.
- Флейки: lib-подмножество `wire/agent/hook/hub::ingress/hub::config` 10 прогонов, `--test ingress_logs` 5 прогонов, 0 падений (`T/scratch/reviewer2/flake.out.txt`).
- Патч `T/scratch/reviewer2/task010.patch` проверен: `git apply --check` в корне репозитория проходит, и на отдельной копии HEAD с `core.autocrlf=true` `git apply` плюс `verify_hashes.sh` дают 11 из 11 `OK`.

Telegram не вызывался, `.env` не читался.

## 1. Summary

В крейте `cctg` появляются четыре модуля, отдельного `proto`-крейта нет. `src/wire.rs` содержит версионированный (`v=1`) newline-JSON протокол agent↔hub (`hello{secret}` → `register` → `reply`/`permission_request`; от hub `registered`/`rejected`/`inbound`/`permission_verdict`), ограниченное чтение строки (1 MiB), `Secret` с редактирующим `Debug` и сравнением через `subtle::ConstantTimeEq`, контракт хука `HookPost`/`HookEvent` с `event_id`, который хук чеканит сам. `src/hub/ingress.rs` содержит TCP-listener агентов (секрет проверяется до `Register`) и самописный строгий HTTP/1.1 endpoint `POST /v1/hook` (Bearer, один `Content-Length` ≤ 1 MiB, без TE, дедлайн 2 с, дедуп по `event_id`: 4096 id / 10 мин, id запоминается только после успешной передачи в канал). `src/agent.rs` делает reconnect с equal-jitter backoff и повторным `Register`, только на стороне агента. `src/hook.rs` делает один POST голым TCP с общим таймаутом, без повторов, и считает успехом только полный `HTTP/1.1 204 ...\r\n`. Hub читает `CCTG_HUB_SECRET` (обязателен), `CCTG_AGENT_LISTEN`/`CCTG_HOOK_LISTEN` (по умолчанию loopback `127.0.0.1:47291/47292`, не-loopback только явным `ip:port`), bind-ит оба listener-а до обращений к Telegram, а события до TASK-011 уходят во временный `drain_ingress`. Реализация это 11 файлов из `REF`, перенесённых патчем или копированием байт в байт.

## 2. Implementation steps

### Step 0. Изоляция и baseline

1. `git status --short -- Cargo.toml Cargo.lock crates` должен быть пуст. Если нет, остановиться и сообщить: патч рассчитан на чистые эти пути.
2. `.env` не открывать, Telegram не вызывать, `~/.claude` не трогать, `main.rs` не менять.
3. Cargo запускать строго по одной команде за раз (памяти на хосте мало), target вне репозитория:
   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
   ```
   Прогонять baseline не обязательно: `REF` уже проверен на том же коде. Если прогнать, ожидание на HEAD: 143 passed, 1 ignored.

### Step 1. Перенести 11 файлов из REF

Основной способ, из корня репозитория:

```bash
git apply --check maw/tasks/in_progress/TASK-010/scratch/reviewer2/task010.patch
git apply maw/tasks/in_progress/TASK-010/scratch/reviewer2/task010.patch
bash maw/tasks/in_progress/TASK-010/scratch/reviewer2/verify_hashes.sh
```

Запасной способ, если `git apply` не проходит: скопировать каждый файл `REF/<path>` → `<path>` байт в байт (руками не править) и запустить тот же `verify_hashes.sh`. Скрипт убирает `\r` перед хэшированием, потому что при `core.autocrlf=true` git может записать файлы в рабочую копию с CRLF, а хэши считаны по LF-байтам. Все 11 строк должны быть `OK`.

| Path | Изменение | Зачем (критерий) | SHA-256 (LF) |
|---|---|---|---|
| `Cargo.toml` | `[workspace.dependencies]` плюс строка `subtle = "2.6"` после `serde_json` | constant-time сравнение секрета; решение оркестратора разрешает прямую зависимость | `b677a4614265304e3e6f51eb4be280bcc26e4f50408596af0b7e9d70fc71b8ff` |
| `Cargo.lock` | только `"subtle",` в dependencies пакета `cctg` (пакет `subtle 2.6.1` уже в lock через rustls, новых пакетов нет) | следствие строки выше | `235aa38d3bf1ed4361da8c7dedcf675d64fd8c1b37f5615fb79244049a4da376` |
| `crates/cctg/Cargo.toml` | `subtle.workspace = true`; tokio features `["io-util", "net", "sync", "time"]` | TCP и io-util для сокетов | `6ab134a126300d49415d10e4f212f134825f69c03853dd7adae3b278127de427` |
| `crates/cctg/src/lib.rs` | `pub mod agent; pub mod hook; pub mod wire;` рядом с `hub` | точки входа для TASK-011/012/013 и integration-тестов | `94eb3c715fdb64ba6f839a8e829c276174d1ea9372d302cfe1d74fd4917e0130` |
| `crates/cctg/src/wire.rs` | новый: константы, `WireError`, `Secret` (`ct_eq`), `AgentMsg`/`HubMsg`, `encode`/`decode`/`read_line`/`write_msg`, `EventId`, `HookPost`/`HookEvent`/`decode_hook`, 11 тестов | критерии 1, 2, 5, 7 | `41fdd57f31b969106af2d41fb9f1ae086ca8f634be26ba8d00e06fcfa109b640` |
| `crates/cctg/src/agent.rs` | новый: `Backoff`, `LinkConfig`, `LinkEvent`, `spawn`, 4 теста включая реальный рестарт hub | критерий 3 | `6fea93e747d06bc5242363d90832ddda199bd582aacffb12afef1b330a78c6a7` |
| `crates/cctg/src/hook.rs` | новый: `PostError`, `post`, строгий `parse_status`, 6 тестов | критерии 4, 5, 7 | `05dee0a88af53649023af14e031daf327f7d6fc8bdca3bfb801ff4da2f459a65` |
| `crates/cctg/src/hub/config.rs` | `SECRET_VAR`, `AGENT_LISTEN_VAR`, `HOOK_LISTEN_VAR`, defaults loopback, `ConfigError::{Secret, ListenAddr}`, поля `hub_secret/agent_listen/hook_listen`, 3 новых теста и один переименованный | критерии 6, 7 | `7e70841fca6a7e09b8e32b3cf924db24ede6061bc39b6973bec0ff96064ff05f` |
| `crates/cctg/src/hub/ingress.rs` | новый: `bind`, `serve_agents`, `serve_hooks`, `Dedup`, строгий HTTP-разбор, lingering close, 16 тестов | критерии 2, 4, 5, 6, 7 | `c4dc5fa3e30bb37c4976f9553366f072ea5d578843455517d1c2ef2ca92a90d8` |
| `crates/cctg/src/hub/mod.rs` | `pub mod ingress;`, в `run()` секрет и два bind до `BotApi`, после commands worker два канала по 256, `serve_agents`, `serve_hooks`, `drain_ingress` | критерий 6, запуск | `ee1986535f446d36caed5ee506d27a375e50779ea555d6ddf8ffec1f368192ab` |
| `crates/cctg/tests/ingress_logs.rs` | новый отдельный тест-бинарник: глобальный TRACE-подписчик, `.without_time()`, все пути ошибок | критерии 2, 7 | `67160ecd5e023f07b5037203c178fc2c649c4e17b9d899707a730240a6a43522` |

Ключевые места, которые это ревью исправило относительно референса планировщика (код уже в `REF`, здесь для понимания):

- `hub/ingress.rs`, `read_request`: версия ровно `version != "HTTP/1.1"` даёт 400 (было `starts_with("HTTP/1.")`); имя поля `!name.bytes().all(is_tchar)` даёт 400 (было «только без пробелов»); новая проверка значения: `value.bytes().any(|b| b.is_ascii_control() && b != b'\t')` даёт 400 (RFC 9110 §5.5: CR/LF/NUL в значении MUST reject-or-replace; до этого голый LF внутри значения проходил).
- `hub/ingress.rs`: `LINGER_BYTES = MAX_HEAD + MAX_HOOK_BODY` (было 64 KiB). Слив идёт фиксированным буфером 4 KiB, не дольше 250 мс.
- `wire.rs`: `constant_time_eq` это `bool::from(a.ct_eq(b))` (было xor-fold + `std::hint::black_box`, у которого по документации нет security-гарантий). Разная длина по-прежнему выходит сразу, так документировано и в `subtle`.
- `hook.rs`, `parse_status`: только `line.strip_suffix(b"\r\n")?.strip_prefix(b"HTTP/1.1 ")?`, затем ровно 3 ASCII-цифры и дальше пусто или пробел. Раньше `HTTP/1.1 204` без CRLF (обрыв) или `HTTP/1.x 204` считались успехом.

### Step 2. Проверка

По одной команде, в таком порядке:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание: fmt и clippy чисто; тесты все зелёные, 184 passed и 1 ignored. Если число другое, это не провал само по себе, но расхождение надо объяснить (например, в HEAD появились новые тесты). Провал это любой failed или новый ignored.

Потом на флейки, без параллельных cargo:

```powershell
1..10 | % { cargo test -p cctg --offline --lib -- wire:: agent:: hook:: hub::ingress:: hub::config:: }
1..5  | % { cargo test -p cctg --offline --test ingress_logs }
```

Все прогоны зелёные. После этого: `git diff --check`, `git status --short` показывает изменёнными ровно 11 файлов из таблицы, `git diff Cargo.lock` это одна добавленная строка `"subtle",`.

### Step 3. Коммит

Сообщение на английском, без trailer-ов `Generated with` / `Co-Authored-By` (закон проекта). Например: `feat(cctg): versioned agent TCP link and hook HTTP ingress (TASK-010)`.

## 3. Test plan

Что закрывает каждый критерий приёмки (все тесты уже в `REF`):

| Критерий | Тесты | Ожидание |
|---|---|---|
| Round-trip всех TCP-сообщений; неизвестная версия/вид без паники | `wire::tests::every_link_message_round_trips_as_one_versioned_line`, `kind_lists_match_the_enums`, `bad_lines_are_typed_errors`, `unknown_fields_are_ignored`, `bad_hook_bodies_are_typed_errors`, `hook_posts_round_trip_and_keep_their_event_id` | каждый вариант обоих направлений и все 7 `HookEvent` round-trip; `v=2`/нет `v`/строковый `v` дают `Version`, неизвестный `type` даёт `UnknownKind`, битые поля и не-JSON дают `Malformed` |
| Неверный secret отвергнут до Register, без логов; длинная строка закрывает соединение с ограниченной аллокацией | `hub::ingress::tests::a_wrong_secret_is_rejected_before_register`, `another_version_is_rejected_as_such`, `an_overlong_line_closes_the_link`, `wire::tests::an_endless_line_stops_at_the_limit`, `tests/ingress_logs.rs` | `rejected{auth}`, событий `Registered` нет; `capacity <= 2*MAX_LINE`; в логах нет секрета |
| Reconnect с backoff и повторный Register после рестарта hub | `agent::tests::the_agent_reconnects_and_registers_again_after_a_hub_restart`, `backoff_grows_to_the_cap_with_jitter`, `a_rejected_secret_keeps_retrying_quietly`, `dropping_the_receiver_stops_the_link` | `Up` → `Down` → 5 попыток с растущими паузами (пятая ≥ 60 мс) → `Up`, hub видит тот же `session_id`, сообщение из очереди на время простоя доходит |
| Один аутентифицированный POST, быстрый ответ, идемпотентность | `hook::tests::a_resent_post_reaches_the_hub_once` (< 500 мс), `hub::ingress::tests::a_repeated_post_is_delivered_once`, `a_full_queue_answers_503_and_the_resend_is_delivered` | повтор даёт 204 и одно событие; 503 не запоминает id, повтор после него доходит |
| `event_id` хука, окно дедупа, натуральные поля не ключ | `wire::tests::event_ids_are_fresh_hex` (10 000 уникальных), `a_repeated_post_is_delivered_once` (resume с тем же `session_id`/`source` это новое событие), `two_stops_of_one_prompt_are_two_events`, `dedup_forgets_by_age_and_by_count` | границы TTL и размера точные |
| Loopback по умолчанию, явный не-loopback | `hub::config::tests::listeners_default_to_loopback`, `non_loopback_listeners_need_an_explicit_address`, `hub::ingress::tests::listeners_bind_loopback_and_explicit_addresses` | defaults разные и loopback; `100.64.0.7:5000` и `[::]:5001` принимаются; `localhost:5000`, без порта, порт вне диапазона дают `ListenAddr(var)` |
| Секрет не логируется ни на одном пути | `tests/ingress_logs.rs`, `wire::tests::errors_and_debug_never_show_the_secret`, `hub::config::tests::a_bad_hub_secret_is_named_but_not_echoed`, `debug_hides_token_user_ids_and_hub_secret`, `hook::tests::no_hub_is_an_error_within_the_timeout` | все 6 ожидаемых строк логов есть (тест не пустой); нет ни настоящего, ни неверного секрета, ни маркера контента; тексты ошибок фиксированные |
| Existing tests pass | `cargo test --workspace --offline` | 184 passed, 1 ignored |

Adversarial HTTP (добавлено этим ревью, всё в `hub::ingress::tests`):

- `bad_requests_get_errors_and_no_event`: плюс к прежним случаям `HTTP/1.x`, `HTTP/1.0`, `HTTP/1.10`; только `Transfer-Encoding`; `Content-Length` вида `+N`, `Nx`, `N,N`, `-N`, два разных; переполнение (26 цифр) даёт 413; `Content-Length : N` (пробел до двоеточия); `Bad(Name`; obs-fold; голые LF, CR и NUL внутри значения. У каждого случая верный bearer и валидное тело, так что статус объясняется только дефектом. Ожидание: 400 (переполнение 413), событий нет.
- `a_body_of_exactly_the_limit_is_accepted`: тело ровно 1 MiB даёт 204 и событие.
- `an_early_401_survives_a_body_of_the_full_limit`: неверный bearer плюс тело 1 MiB, клиент читает целый 401 (регрессия на RST на Windows).
- `one_connection_carries_at_most_one_event`: два запроса одной записью дают 400 и 0 событий, либо 204 и ровно одно (зависит от сегментации TCP); второй запрос после ответа никогда не читается.
- `a_slow_request_is_closed_at_the_deadline_without_an_event`: незавершённый заголовок закрывается примерно через 2 с без ответа и без события.

Hook client (`hook::tests`):

- `status_lines`: принимаются `HTTP/1.1 204 No Content\r\n`, `HTTP/1.1 204\r\n`; отвергаются строка без CRLF, только LF, `HTTP/1.x`, `HTTP/1.0`, 2 или 4 цифры, нецифры, двойной пробел, чужой протокол, пусто.
- `a_truncated_or_foreign_status_line_is_not_success`: фейковый сервер отвечает `HTTP/1.1 204` и закрывает, `HTTP/1.x 204`, `HTTP/1.1 2040`, пустой ответ, и во всех случаях `Err(BadResponse)`; нормальный ответ даёт `Ok(())`.
- `a_wrong_secret_is_refused` (читаемый 401), `a_silent_hub_costs_at_most_the_timeout`, `no_hub_is_an_error_within_the_timeout`.

Мутационная проверка (уже выполнена, повторять implementer не обязан): `T/scratch/reviewer2/mutate.py` вносит по одной мутации, гоняет нужный тест и откатывает файл. Мутации R1–R5 и R9 возвращают ровно дефектный код планировщика, поэтому их гибель одновременно воспроизводит каждый дефект и доказывает, что тест его ловит. Результат: 19 из 19 KILLED.

## 4. Rollout notes

- **Новая обязательная переменная.** После мержа `cctg hub` не стартует без `CCTG_HUB_SECRET` (16+ видимых ASCII, без пробелов). По `OPEN_DECISIONS.md` оркестратор сам добавит случайное значение в локальный `.env` при мерже и не будет его печатать. Implementer `.env` не читает и не меняет. Ошибка старта называет только имя переменной.
- **Порты.** Hub занимает `127.0.0.1:47291` (агенты) и `127.0.0.1:47292` (хуки) до любых вызовов Telegram. Занятый порт роняет старт с текстом, где названа переменная для переопределения. Переопределение через `CCTG_AGENT_LISTEN`/`CCTG_HOOK_LISTEN` только как `ip:port`, host-имена не резолвятся. Не-loopback пишет warn при bind: трафик это plain TCP, только для Tailscale или частной LAN.
- **Зависимости.** Добавлена одна прямая зависимость `subtle` (уже была в дереве транзитивно, 2.6.1), это одобрено оркестратором. `Cargo.lock` меняется на одну строку. Сеть при сборке не нужна (`--offline` работает).
- **Миграций и feature flags нет.** Протокол `v=1` нигде ещё не развёрнут. Новое опциональное поле добавляется с `#[serde(default)]` без смены версии, новый тип сообщения или смена смысла поднимают `VERSION`.
- **Потребители, которых пока нет.** `drain_ingress` временный: события агентов и хуков только логируются (номер соединения), до TASK-011. `agent::spawn` и `hook::post` подключат TASK-013 и TASK-012. Как агент и хук узнают адрес и секрет, решает TASK-012/013; единственный повтор в хуке тоже TASK-012 (`OPEN_DECISIONS.md`).
- **Известные ограничения, принятые сознательно:**
  - Нет keepalive/ping для удалённых агентов (решение: шаг «второе устройство»).
  - Сообщение агента, запись которого упала на разрыве, теряется (at-most-once). Для `permission_request` терминальный диалог остаётся.
  - Окно дедупа живёт только в памяти: после рестарта hub, 10 минут или 4096 событий поздний повтор пройдёт снова.
  - Слив при раннем ответе ограничен: клиент, который шлёт больше 1 MiB или медленнее 250 мс, может получить reset. Это вне контракта.
  - До аутентификации hub читает первую строку агента вплоть до `MAX_LINE`. В худшем случае это 256 соединений × 1 MiB = 256 MiB, по умолчанию доступно только с loopback. Если понадобится, отдельный меньший лимит на handshake это отдельная задача.
  - Equal jitter выбран ради ненулевой минимальной паузы. AWS по времени выполнения предпочитает full jitter, пересмотреть при нагрузке с нескольких устройств.
  - Секрет лежит в памяти в `String` без zeroize, модель угроз локальная.

## 5. Review notes

### Disconfirmation (записан до оценки: `T/scratch/reviewer2/disconfirmation.md`)

Контрпример: `hook::post` возвращает `Ok(())` («hub has the event») на неполной или чужой строке статуса, например сервер пишет `HTTP/1.1 204` и закрывает соединение, или пишет `HTTP/1.x 204\r\n`. Второй поиск: любой путь, по которому секрет попадает в лог или в текст ошибки.

Результат: **первый контрпример подтвердился.** В референсе планировщика и в `reviewer1-ws` (reviewer-1 не трогал `hook.rs`) `parse_status` возвращал `Some(204)` для обоих случаев. Воспроизведено мутацией R5: возврат к коду планировщика валит `status_lines` и `a_truncated_or_foreign_status_line_is_not_success`. Исправлено. Второй поиск ничего не нашёл: `Secret::expose()` вызывается ровно в двух местах, это заголовок в `hook::post` (уходит только в сокет) и сравнение в `agent_session`. Все типы ошибок (`WireError`, `SecretError`, `PostError`, `ConnectError`, `ConfigError::{Secret, ListenAddr}`) несут фиксированный текст, `ErrorKind` или числа. `Secret`, `LinkConfig`, `Config` в `Debug` редактируют секрет. `#[instrument]` нигде нет. Ответы HTTP и `rejected` фиксированные. В `ingress_logs` добавлены два запроса с секретом в неверной версии HTTP, в имени заголовка и в значении с голым LF, логи по-прежнему чистые.

### Что изменено относительно PLAN_V2 и почему

1. **План снова указывает на собранный и проверенный референс (`REF`) с хэшами и применимым патчем**, а не на описание «сделать по мотивам». PLAN_V2 правильно отверг старые `hashes.txt`/`final.diff` (дефектный код, пути `b/maw/...`), но оставил implementer-у дописывать исправления и тесты руками. Теперь всё сделано и проверено один раз: `task010.patch` применяется из корня (проверено на копии с `autocrlf=true`), `verify_hashes.sh` не зависит от CRLF.
2. **Воспроизведены все дефекты из PLAN_V2**, каждый мутацией, которая возвращает код планировщика: R1 `HTTP/1.x` (падает `bad_requests…`), R2 слив 64 KiB (падает `an_early_401…`), R3 имя поля не по `tchar`, R5 статус хука. `black_box` тестом не ловится по природе; заменён на `subtle::ConstantTimeEq`, документация `subtle` подтверждает short-circuit только по длине. R8 проверяет, что сравнение вообще смотрит на байты.
3. **Новый дефект, которого не было в PLAN_V2:** голые CR, LF, NUL (и прочие CTL, кроме HTAB) внутри значения заголовка принимались. RFC 9110 §5.5 требует reject-or-replace. Исправлено отказом 400, тест в `bad_requests…`, мутация R4 убита.
4. **Тесты reviewer-1 (`tests/reviewer_http_adversarial.rs`) не переносятся отдельным бинарником.** Они переписаны внутри `hub::ingress::tests` на общих хелперах: каждый integration-бинарник это отдельная линковка на хосте с тесной памятью. Тест pipelining у reviewer-1 проверял только `≤ 1` событие. Теперь он требует соответствия статусу (400 → 0 событий, 204 → 1) и отдельно проверяет, что второй запрос после ответа не читается. Slowloris теперь проверяет, что соединение держится до дедлайна 2 с.
5. **Добавлены недостающие adversarial-случаи из PLAN_V2 Step 4:** `+N`, `Nx`, `N,N`, `-N`, разные дубли CL, переполнение → 413, только TE, `HTTP/1.0`/`HTTP/1.10`, obs-fold, ровно 1 MiB тела → 204 (R6), дедлайн (R7), дубль CL (R9).
6. **`subtle` объявлен через `[workspace.dependencies]`** (`subtle = "2.6"`, в крейте `subtle.workspace = true`), как все остальные зависимости `cctg`. У reviewer-1 был прямой pin `2.6.1` в крейте. Lock одинаковый в обоих случаях.
7. **Не взято из PLAN_V2:** запись заголовка и тела хука двумя `write_all`. Одна запись с concat не ошибка, менять только ради стиля задача запрещает. Число тестов снова указано (184 + 1 ignored), но как ориентир: провал это только failed или новый ignored.
8. **Старое предложение в `PCTX_PROPOSALS.md` про 64 KiB исправлено** отдельной записью, плюс добавлен урок про строгий HTTP-подмножество.

Решения этого этапа записаны в `T/log.jsonl` (stage `plan-reviewer-2`).

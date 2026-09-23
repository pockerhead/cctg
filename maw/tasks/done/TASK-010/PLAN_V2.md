# PLAN V2 — TASK-010: transport contracts, agent TCP и hook HTTP ingress

Stage: plan-reviewer-1 (codex/gpt-5.6-sol, effort=medium). Пути указаны от корня репозитория `C:/Users/user/dev/cctg`.

## 1. Review notes

### Что проверено

- Обязательный disconfirmation-контрпример был зафиксирован до оценки: `Content-Length : 0` (пробел перед двоеточием). Он **не подтвердился**: `hub/ingress.rs:470-472` отвергает whitespace в имени заголовка и возвращает 400. Evidence: `maw/tasks/in_progress/TASK-010/scratch/plan-reviewer-1-disconfirmation.md`.
- Референс скопирован в `maw/tasks/in_progress/TASK-010/scratch/reviewer1-ws` и собран независимо с `CARGO_TARGET_DIR` под `%TEMP%`. Исходная версия прошла `cargo test --workspace --offline`: 179 passed, 1 ignored.
- Проверены реальные файлы HEAD и все девять заявленных файлов референса; HEAD кода соответствует базе `b417b00` (последующий commit `7261956` добавляет план и референс, не меняя `Cargo.toml`, `Cargo.lock` или `crates/**`). `.env` и значения секретов не читались, Telegram не вызывался.

### Ошибки исходного плана

1. **HTTP parser принимает синтаксически неверную версию.** В `hub/ingress.rs:451-458` условие `version.starts_with("HTTP/1.")` принимает `HTTP/1.x`, `HTTP/1.` и многосимвольный minor. Добавленный в scratch тест отправил валидный hook body с `HTTP/1.x`; референс ответил `204 No Content` и передал событие в hub. RFC 9112 §2.3 задаёт `HTTP-version = HTTP-name "/" DIGIT "." DIGIT`; для этого узкого endpoint клиент и сервер должны требовать ровно `HTTP/1.1`. Поэтому утверждение плана, что ручной parser закрывает все опасные ветки, неверно. Источник: [RFC 9112 §2.3](https://www.rfc-editor.org/rfc/rfc9112.html#section-2.3).

2. **Ранний HTTP-ответ действительно теряется из-за reset.** `LINGER_BYTES=64 KiB` (`hub/ingress.rs:33-35`) недостаточен для разрешённого тела в 1 MiB. На Windows запрос с неверным bearer и `Content-Length = MAX_HOOK_BODY` был полностью записан клиентом, но чтение ответа завершилось `ConnectionAborted` без байтов 401. Это прямо воспроизводит риск из исходного плана; считать reset эквивалентом нормального HTTP-ответа нельзя, поскольку `hook::post` теряет статус и диагностируемость. В scratch увеличение bounded drain до `MAX_HEAD + MAX_HOOK_BODY` (чтение фиксированным 4 KiB буфером, deadline 250 ms) сохранило 401 и прошло тест.

3. **`std::hint::black_box` не даёт заявленной security-гарантии constant-time.** `wire.rs:101-108` называет сравнение constant-time, но стандартная библиотека прямо говорит, что `black_box` нельзя использовать для корректности или криптографических/security-гарантий. Это не вопрос тестового тайминга: гарантия выбранного primitive отсутствует по его контракту. Следует использовать `subtle::ConstantTimeEq`; `subtle 2.6.1` уже присутствует в lockfile транзитивно через rustls, но его добавление как прямой зависимости всё равно обновит список зависимостей пакета `cctg` в `Cargo.lock`. Источники: [Rust `black_box`](https://doc.rust-lang.org/std/hint/fn.black_box.html), [`subtle::ConstantTimeEq`](https://docs.rs/subtle/latest/subtle/trait.ConstantTimeEq.html).

4. **Имена HTTP headers валидируются неполно.** Текущий код отвергает whitespace, но принимает прочие символы вне `token` (например, `Bad(Name`) и затем игнорирует такой malformed header. RFC grammar требует `field-name = token`. Нужна небольшая `is_tchar`-проверка и негативный тест; это исправление в scratch прошло.

5. **Клиентский parser статуса слишком снисходителен.** `hook.rs:71-78` принимает любую строку, начинающуюся с `HTTP/1.`, и не требует завершённой строки. Например, `HTTP/1.x 204` или EOF после `HTTP/1.1 204` может ошибочно означать успешную доставку. Поскольку `Ok(())` документирован как «hub has the event», принимать неполный/невалидный status-line нельзя. Требовать завершённый CRLF status-line, ровно `HTTP/1.1` и ровно три ASCII digits.

6. **Adversarial HTTP coverage неполный.** Есть тесты CL+TE и повторного `Content-Length`, но нет malformed version, полного `token` для field-name, разных malformed CL (`+1`, `1x`, `1,1`, разные duplicate values), pipelining, slowloris и сохранности раннего ответа при максимальном допустимом теле. Директива задачи требует проверить именно эти случаи. Пять scratch-тестов (malformed version/header, early response, pipelining, slowloris) проходят после точечных исправлений.

7. **Ссылка на AWS неточна.** Реализация — Equal Jitter `[d/2,d]`; AWS действительно описывает этот вариант, но называет его худшим из jitter-вариантов по времени выполнения и предпочитает Full Jitter. Для этого контракта Equal Jitter можно сохранить как осознанный способ гарантировать ненулевую паузу и исключить tight loop, но не следует утверждать, что AWS рекомендует именно его. Источник: [AWS Exponential Backoff and Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/).

8. **`final.diff` не является применимым patch против корня.** Его `b/...` paths указывают на `maw/tasks/.../scratch/planner/ws/...`; `git apply --check` пытается создать уже существующие scratch-файлы. Кроме того, hashes относятся к версии с найденными дефектами. Revised plan не должен предлагать byte-for-byte copy или проверку старых hashes как финальное действие.

9. **Жёстко ожидать конкретное число тестов хрупко.** Базовое число 179 подтверждено, но после обязательных regression-тестов оно вырастет. Критерий — успешный полный прогон и отсутствие ignored сверх уже известного изолированного config-теста, а не фиксированное число.

## 2. Updated understanding

- HEAD содержит один workspace и бинарник `cctg`; `main.rs` уже парсит `hub`, `agent`, `hook`, но последние две ветки пока пустые. TASK-010 добавляет библиотечные transport primitives, а wiring CLI остаётся TASK-012/013. `main.rs` менять не надо.
- `crates/cctg/src/lib.rs` сейчас экспортирует только `hub`; нужны `agent`, `hook`, `wire`.
- `hub::run` сейчас загружает config/offset, проверяет Telegram-права, запускает scheduler/commands и polling. Listener-ы надо bind-ить до Bot API вызовов, а до TASK-011 их bounded channels потребляет временный `drain_ingress` без логирования payload/path.
- Agent transport — versioned newline-delimited JSON over persistent TCP. Первая строка — `hello{secret}`, вторая — `register`; только после обеих hub публикует `AgentEvent::Registered`. Reconnect/backoff находится только в agent.
- Hook transport — один HTTP/1.1 `POST /v1/hook` на соединение, без retry loop в TASK-010. `HookPost::new` один раз генерирует случайный `event_id`; повтор той же структуры сохраняет id. Hub dedup хранит до 4096 id не дольше 10 минут и вставляет id только после успешного `try_send`.
- Решения `OPEN_DECISIONS.md` окончательны: поиск адреса/секрета остаётся TASK-012/013; keepalive удалённых агентов — шаг разработки второго устройства; единственный retry hook — TASK-012. В этой задаче API принимает `addr` и `Secret`, TCP keepalive и hook retry не добавляются. Оркестратор сам добавит случайный `CCTG_HUB_SECRET` в локальный `.env` при merge; implementer `.env` не читает и не меняет.
- Заявленная секретность логов в референсе в целом реализована правильно: `WireError`, `PostError`, network errors и config errors содержат только фиксированный текст/`ErrorKind`; отдельный `ingress_logs` test активирует TRACE subscriber с `.without_time()` и проверяет реальные error paths.
- Заявленные dedup, 503-then-retry, distinct resume/Stop events, restart/re-register, bounded newline и loopback/non-loopback tests существуют и прошли. Их нужно сохранить, а не перепроектировать.

## 3. Revised approach

Сохранить архитектуру референса — четыре небольших модуля внутри `cctg`, без отдельного `proto` crate и без HTTP framework — но не копировать её byte-for-byte. Внести только доказанные исправления.

1. **Wire contract (`wire.rs`).** Оставить `VERSION=1`, typed enums, двухфазный `Value` decode, fixed-text `WireError`, `MAX_LINE=1 MiB`, `HookPost`/`HookEvent`, `EventId` и bounded read. Заменить самописный `black_box` compare на `subtle::ConstantTimeEq` для равных по длине slices (length leak допустим и документирован библиотекой). `Secret` остаётся redacted в `Debug`; serde errors никогда не выходят наружу.

2. **Hub ingress (`hub/ingress.rs`).** Сохранить handshake state machine, bounded connection counts, channels и dedup. Для HTTP:
   - принимать ровно `POST /v1/hook HTTP/1.1`;
   - валидировать каждое имя header по RFC `token`/`tchar` и reject obs-fold/whitespace-before-colon;
   - требовать ровно один decimal `Content-Length <= 1 MiB`; любой `Transfer-Encoding`, combined/duplicate/malformed CL отвергать;
   - держать header buffer и body allocation bounded, один общий read deadline 2 s;
   - не обрабатывать второй pipelined request;
   - после раннего ответа делать shutdown(write) и bounded/time-limited drain до `MAX_HEAD + MAX_HOOK_BODY`, чтобы собственный in-limit hook client получил статус без Windows RST; oversized/slow hostile peers всё равно ограничены 250 ms и фиксированным drain buffer.

3. **Agent (`agent.rs`).** Сохранить connect timeout, hello+register on every connection, `Up`/`Down`, bounded queues и exponential Equal Jitter. Описать Equal Jitter как сознательный minimum-delay trade-off, не как предпочтительный алгоритм AWS. Reconnect test должен доказывать restart, повторный Register с тем же session id и отсутствие tight loop без вероятностно хрупкого ожидания точных случайных интервалов.

4. **Hook client (`hook.rs`).** Один POST и один общий timeout, без retry. Писать header/body отдельными `write_all`, чтобы не создавать дополнительную concat-копию. Status parser принимает только завершённый `HTTP/1.1 <3 digits> ...\r\n`; только 204 означает успех. Все ошибки фиксированы и не содержат addr, secret, body или response text.

5. **Config/startup.** Defaults остаются `127.0.0.1:47291/47292`; только explicit numeric `SocketAddr` может открыть non-loopback, при bind пишется warning без секретов. Hub требует `CCTG_HUB_SECRET`, bind-ит оба listener-а до Telegram requests и запускает временный drain до TASK-011.

6. **Dependencies.** Добавить `subtle = "2.6.1"` в `[workspace.dependencies]`, `subtle.workspace = true` в `crates/cctg/Cargo.toml`, а также tokio features `io-util`/`net`. `Cargo.lock` изменится только в dependency list пакета `cctg`; новый package в lock не появляется, потому что `subtle 2.6.1` уже транзитивно присутствует.

## 4. Revised steps

### Step 0 — изоляция и baseline

1. Проверить `git status --short -- Cargo.toml Cargo.lock crates`. Не требовать пустого всего worktree: сохранить любые пользовательские изменения и остановиться только при пересечении с перечисленными ниже файлами, которое нельзя безопасно совместить.
2. Не читать `.env`, не обращаться к Telegram, не менять `~/.claude`.
3. Назначить отдельный target вне repo и запускать только одну Cargo-команду одновременно:

   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
   cargo test --workspace --offline
   ```

4. Зафиксировать baseline: на проверенной базе 179 passed, 1 ignored; это ориентир, не финальный hard-coded count.

### Step 1 — dependencies и module surface

Изменить:

- `Cargo.toml`: workspace dependency `subtle = "2.6.1"`;
- `Cargo.lock`: принять только ожидаемое добавление `"subtle"` в dependencies пакета `cctg`;
- `crates/cctg/Cargo.toml`: tokio features `io-util`, `net`, `sync`, `time`; `subtle.workspace = true`;
- `crates/cctg/src/lib.rs`: экспортировать `agent`, `hook`, `wire`, сохранив `hub`.

Не добавлять отдельный proto crate, hyper/axum/rmcp, не менять `main.rs`.

### Step 2 — versioned wire contracts

Создать `crates/cctg/src/wire.rs` на основе проверенного референса, но с `subtle::ConstantTimeEq` вместо `std::hint::black_box`.

- Agent→hub: `hello`, `register`, `reply`, `permission_request`.
- Hub→agent: `registered`, `rejected`, `inbound`, `permission_verdict`.
- Каждая TCP line содержит `v=1`, `type`, завершается одним `\n`; unknown version → `WireError::Version`, unknown type → `UnknownKind`, malformed fields/JSON → `Malformed`, без входного текста в errors.
- `read_line` ограничивает чтение `MAX_LINE`, принимает ровно limit с newline, закрывает over-limit/no-newline peer и не раздувает buffer без границы.
- `Secret::parse` принимает 16+ visible ASCII; `Debug` redacted; `matches` вызывает `ct_eq` (с documented length short-circuit).
- `HookPost::new` создаёт свежий 32-hex `EventId`; повторная отправка клонированного `HookPost` сохраняет id. Естественные payload fields не участвуют в id.

Tests в модуле:

- round-trip каждого варианта обоих направлений и каждого `HookEvent`;
- missing/string/unknown version, unknown type, malformed fields, garbage/empty input — typed error, no panic;
- unknown optional fields tolerated;
- secret marker не появляется в `Display`/`Debug` parse errors или enum Debug;
- equal/different secret, включая разные длины;
- endless line, exact limit, truncated EOF и bounded capacity;
- 10 000 generated ids unique/well-formed, malformed id rejected.

### Step 3 — agent listener and client reconnect

Создать `crates/cctg/src/agent.rs` и agent half `crates/cctg/src/hub/ingress.rs`.

- Hub читает и проверяет только первую line как hello; при wrong secret/Register-first отправляет fixed `rejected{auth}`, не читает/не публикует Register и закрывает connection.
- Вторая line обязана быть Register; только затем hub публикует `Registered` и отвечает `registered`.
- Over-limit/version change закрывают link; malformed/unknown post-register lines дают fixed warning и не паникуют; disconnect публикуется один раз.
- Agent на каждом connect шлёт hello+тот же Register, ждёт registered, затем двунаправленно передаёт messages. Reconnect/backoff только здесь; hook не использует этот loop.
- Сохранить bounded mpsc queues и connection semaphore.

Tests:

- wrong secret и Register-first не создают `AgentEvent::Registered`; secret отсутствует в ответах/logs;
- unknown version/type controlled, over-limit stream закрывается с bounded buffer;
- двунаправленный обмен и Disconnect;
- реальный restart: остановить listener/connections, увидеть Down, несколько reconnect attempts с backoff, поднять hub на том же port, увидеть Up и повторный Register с тем же session id; queued-while-down message доходит;
- rejected secret не даёт Up и не спамит warning; drop owner останавливает link.

### Step 4 — strict hook HTTP ingress and dedup

Завершить hook half `crates/cctg/src/hub/ingress.rs`.

- `POST /v1/hook HTTP/1.1`, exact request-line grammar для поддерживаемого subset.
- Header cap 8 KiB; `field-name` только non-empty RFC `tchar`; whitespace before colon/obs-fold/invalid UTF-8 reject.
- Ровно один decimal Content-Length; reject TE при любом CL, duplicate/combined/different CL, sign/non-digit/overflow; 411 без CL, 413 above body cap.
- Authorization comparison происходит до semantic body decode; response/error/log не содержит credential/body/path.
- Read header+body under 2 s deadline; slowloris connection закрывается без event.
- Один request per connection; pipelined bytes никогда не дают второе event.
- Early response uses `Connection: close`, shutdown write and fixed-buffer drain capped at `MAX_HEAD + MAX_HOOK_BODY`/250 ms.
- Dedup check + `try_send` + insert остаются atomic под mutex; duplicate → 204/no event; full/closed channel → 503/no insert; insert only after successful handoff.

Tests (включая новые regression cases):

- valid authenticated POST быстро даёт 204/event;
- wrong/missing auth, bad JSON, unknown event type;
- exact rejection of `HTTP/1.x`, invalid header token, whitespace-before-colon и obs-fold;
- CL+TE, TE only, duplicate equal/different CL, `1,1`, `+1`, `1x`, overflow;
- header/body boundary, fragmented body, exact 1 MiB body, over-limit body;
- slowloris deadline и pipelined second request (не более одного event);
- wrong bearer plus full allowed body still yields readable 401 on Windows (regression for RST);
- same `event_id` delivered once; two separately minted SessionStart/resume with same natural fields both delivered; two Stop for same prompt both delivered;
- dedup exact TTL/count bounds; 503 then retry delivers once.

### Step 5 — hook client

Создать `crates/cctg/src/hook.rs`.

- Serialize one `HookPost`, write HTTP header and body separately, use one timeout over connect/write/read; no internal retry loop.
- Parse at most 256 bytes, require terminating CRLF, exact `HTTP/1.1`, exactly three-digit status; success only on 204.
- `PostError` carries only timeout, `ErrorKind`, numeric status or fixed malformed-response error.

Tests:

- same `HookPost` sent twice reaches hub once and returns quickly;
- wrong secret → readable 401; silent/absent hub respects timeout;
- reject `HTTP/1.x 204`, truncated `HTTP/1.1 204` without CRLF, non-three-digit code and overlong status line;
- `Display`/`Debug` errors contain no secret, body, addr or response text.

### Step 6 — config and hub startup

Изменить `crates/cctg/src/hub/config.rs` и `crates/cctg/src/hub/mod.rs`.

- Добавить redacted optional `hub_secret`, numeric SocketAddr listener fields и defaults.
- Invalid config errors называют только env variable; values не эхоятся.
- В `run`: load config/state, require secret, bind agent and hook listener before any Telegram call, затем прежние startup checks/workers, ingress tasks и temporary bounded `drain_ingress`.
- Non-loopback допустим только через explicit `CCTG_AGENT_LISTEN`/`CCTG_HOOK_LISTEN`; `bind` выдаёт fixed warning. Hostnames intentionally unsupported.

Tests:

- defaults distinct and loopback;
- explicit Tailscale IPv4/IPv6-any accepted; hostname/missing port/out-of-range rejected without echo;
- invalid/missing hub secret named but not echoed; Config Debug hides bot token, ids and hub secret;
- occupied listener produces startup context naming only relevant variable (без Telegram вызова).

### Step 7 — end-to-end secret log test

Создать/обновить `crates/cctg/tests/ingress_logs.rs` как отдельный integration-test binary с одним global TRACE subscriber и `.without_time()`.

Прогнать wrong secret, malformed hello with marker, version mismatch, over-limit line, registered unknown/malformed/reply payloads, bad bearer, malformed/unknown hook body, invalid HTTP version/header, valid hook и duplicate. Assert:

- ожидаемые log callsites реально сработали (тест не vacuous);
- отсутствуют real/wrong secrets, content marker, host/cwd/transcript path;
- ошибки не содержат raw serde input.

### Step 8 — acceptance mapping and verification

Проверить критерии напрямую:

| Acceptance criterion | Concrete proof |
|---|---|
| TCP serde + controlled unknowns | `wire` round-trip/error tests |
| Auth before Register; bounded long line; no secret logs | agent handshake/limit tests + `ingress_logs` |
| Reconnect/backoff/re-register | hub restart integration test |
| Fast authenticated hook + idempotence | hook POST timing + duplicate event test |
| Random per-invocation event_id; bounded window; natural fields distinct | EventId, resume/Stop, TTL/count, 503 retry tests |
| Loopback default / explicit non-loopback | config + bind tests |
| Secret absent on every parse/error path | isolated TRACE log test + fixed error type tests |
| Existing tests | full workspace test |

Запустить по одной команде:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task010-impl-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Затем 10 раз прогнать transport lib subset и 5 раз `--test ingress_logs`; не запускать параллельные Cargo processes. Проверить `git diff --check`, `git diff --stat`, отсутствие неожиданных изменений вне перечисленных файлов и точный `Cargo.lock` diff. Не использовать старые `hashes.txt`/`final.diff` как критерий корректности.

Финальный touched set:

- `Cargo.toml`
- `Cargo.lock`
- `crates/cctg/Cargo.toml`
- `crates/cctg/src/lib.rs`
- `crates/cctg/src/wire.rs`
- `crates/cctg/src/agent.rs`
- `crates/cctg/src/hook.rs`
- `crates/cctg/src/hub/config.rs`
- `crates/cctg/src/hub/ingress.rs`
- `crates/cctg/src/hub/mod.rs`
- `crates/cctg/tests/ingress_logs.rs`

`main.rs`, `.env`, Telegram state, `~/.claude`, transcript crate и остальные hub modules не менять.

## 5. Risk areas

- **Plain TCP outside loopback.** Shared secret authenticates but does not encrypt traffic. Explicit non-loopback is intended only for Tailscale/private LAN; warning remains. TLS is out of scope.
- **Half-open remote TCP.** Без keepalive/ping hub/agent могут поздно заметить sleep/network partition. По решению оркестратора это deferred до шага второго устройства, не TASK-010.
- **At-most-once gap for agent outbound writes.** Message whose socket write fails can be lost; TASK-010 does not add acknowledgements. Permission terminal dialog remains available. Do not silently claim exactly-once delivery.
- **Hook retry ownership.** TASK-010 exposes stable `HookPost.event_id` and single-shot `post`, but retry policy remains TASK-012 per orchestrator decision; no retry loop here.
- **Dedup window is intentionally finite and volatile.** After 10 minutes, >4096 accepted events, or hub restart, a very late retry may pass again. Tests must assert exact boundary semantics; durable dedup is outside scope.
- **Early-response drain remains bounded.** Full in-limit requests are drained sufficiently to preserve status on Windows; clients sending more than the 1 MiB contract or trickling past 250 ms can still observe reset. This is acceptable for hostile/out-of-contract peers and prevents resource capture.
- **Constant-time is best effort at software/hardware level.** `subtle` is materially stronger and purpose-built compared with `std::hint::black_box`, but its own documentation correctly avoids absolute side-channel guarantees. Secret length remains observable.
- **Equal Jitter trade-off.** It guarantees a minimum delay and prevents a deterministic tight retry loop, but AWS measurements favor Full Jitter under synchronized contention. Revisit only when multi-device load justifies it; do not broaden TASK-010.
- **Live hub configuration.** After merge hub requires `CCTG_HUB_SECRET` and two free ports. Per `OPEN_DECISIONS.md`, the orchestrator adds a random local value at merge without printing it; implementer does not touch `.env`.
- **Preliminary hook schema.** `claude_pid`/`parent_claude_pid` may be refined by TASK-012 before deployment, but the current fields match the hub-registry nesting contract. Any later optional field uses `#[serde(default)]`; semantic/type changes require a version decision before rollout.

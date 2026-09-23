# IMPL_REVIEW — TASK-008

Stage: code-reviewer (claude/opus, effort=medium). Ревью по коду из `git diff main -- Cargo.toml Cargo.lock crates/` и по моим собственным прогонам, не по IMPL_SUMMARY. Реальный Telegram не вызывался, `.env` не читался. Target dir вне репозитория (`%TEMP%/cctg-crev008-target`).

## 1. Verdict

**PASS**: все 9 критериев приёмки подтверждены кодом и тестами. Найдены три minor-проблемы (голодание message-полосы под потоком правок, нет нижней границы у `retry_after = 0`, токен попадает в env процесса). Критерии они не ломают.

## Disconfirmation (первым делом)

Контрпример, записанный до ревью: "в каком-то 60-секундном окне планировщик делает 21 metered-попытку, потому что refill копится во время стартового burst или потому что повтор после 429 уходит без списания токена".

Проверка:
- Аналитически: `Bucket` (`scheduler.rs:189-225`) непрерывный, ёмкость 5, 1 токен за 4 с, cap на ёмкости. k-я отправка от момента полного bucket требует `5 + Δt/4 ≥ k`, значит 20-я не раньше чем через 60 с. `dispatch` списывает токен до вызова transport (`scheduler.rs:407-409`), повтор после 429 проходит через `pick` и списывает токен снова.
- Эмпирически: scratch-тест `crev_random_arrivals_and_429_keep_window` (`scratch/crev/ws/.../scheduler.rs`): 200 отправок в 4 темы со случайными интервалами 0..9 с, примерно 20% permission, 10 ответов 429 по 1..10 с. Итог: 210 попыток, максимум **20** в *замкнутом* окне 60 с, промежуток везде ≥ 1 с, порядок каждой темы сохранён.

**Контрпример не подтвердился.**

## 2. Confirmed correct

- **Критерий 1, allowlist и утечки.** `updates.rs:87-136`: message и callback вне allowlist дают `Ignored::NotAllowed`. В `Inbound`/`CallbackInput` нет user id. Логи роутинга пишут только `reason`/`kind` (`updates.rs:157-161`). Каждый `reqwest::Error` проходит через `without_url()` (`api.rs:37-41`), включая ошибку `.bytes()` (`api.rs:338`). Тело разбирается руками, `Response::json`/`error_for_status` нигде не вызываются. У `BotApi`, `BotToken`, `Allowlist` ручной `Debug` (`api.rs:148`, `config.rs:43,65`). Ошибка `dotenvy` не хранится, `reason` берётся из трёх фиксированных строк (`config.rs:142-151`). Тесты `transport_errors_never_contain_the_token` (настоящий connect-error, anyhow `{:#}`), `malformed_env_file_does_not_echo_its_contents` (stderr настоящего CLI) и `routing_logs_never_contain_user_ids` проходят. Stdout: tracing пишет в stderr (`main.rs:42-47`). В не-тестовом коде нет `println!`/`dbg!`/`unwrap`/`expect` (grep).
- **Критерий 2, 20/мин и FIFO.** Подтверждено выше. `next_permission` (`scheduler.rs:323-341`) выбирает промпт, только если в `message` нет более раннего `Send`/`SendDocument` его темы, `None` (General) тоже считается темой. `push_front` после 429 корректен: у выбранного промпта нет более старых заданий его темы.
- **Критерий 3, коалесинг и 429.** Правка того же `message_id` переписывает текст и markup уже стоящей в очереди правки на её месте, прежний waiter получает `Superseded` (`scheduler.rs:343-367`). 429 ставит на паузу всю очередь до `retry_after`, одна попытка на каждый `retry_after`: тест `repeated_429_is_one_attempt_per_retry_after` даёт попытки в 0, 3, 6, 9 с.
- **Критерий 4, полосы.** `metered()` покрывает только `Send`/`SendDocument` (`scheduler.rs:85-87`). У `Edit`/`Topic` нет ни константы, ни конфига лимита. `FALLBACK_RETRY_AFTER` используется только для ответа 429.
- **Критерий 5, служебные сообщения.** Все четыре `forum_topic_*` распознаются до проверки `from`/allowlist и возвращаются как `Routed::Service` (`updates.rs:92-98`). В `mod.rs:72` на них нет обработчика ввода.
- **Критерий 6, толерантный поллинг.** `getUpdates` возвращает `Vec<Value>`, каждый апдейт разбирается отдельно, offset двигается по любому целому `update_id` (`updates.rs:141-165`). `poll` никогда не выходит: на 429 спит `retry_after`, на прочие ошибки backoff 1→30 с.
- **Критерий 7, can_manage_topics.** `run` делает `getMe` → `getChatMember(bot id)` → `check_topic_rights` до spawn планировщика и поллинга (`mod.rs:43-57`). В тексте ошибки есть `can_manage_topics` и "Manage Topics".
- **Критерий 8, измерения.** IMPL_SUMMARY §3: release 5,490,688 B (сверено с `scratch/release_measure_implementer.out.txt`), clean build 45.6 с и 91 с с указанием условий, idle ≈18.7 MB WS. RSS под живым long poll перенесён в TASK-009 решением оркестратора.
- **Критерий 9 и гигиена.** Мои прогоны: `cargo clippy --workspace --all-targets --offline -- -D warnings` чистый, `cargo fmt --check` чистый, `cargo test --workspace --offline` даёт **103 passed** (27+1+1+3 cctg, 70+1 transcript). Флейки: lib binary и `routing_logs` по 40 раз, stdout 10 раз, **0 падений**. `cargo tree --depth 1`: ровно 10 разрешённых крейтов, `teloxide`/`rmcp` нет. В коммитах ветки нет трейлеров "Generated with"/"Co-Authored-By". Все 12 файлов соответствуют PLAN_FINAL.

## 3. Issues

### minor-1: поток правок бесконечно держит обычные сообщения
`crates/cctg/src/hub/scheduler.rs:386-397`. В `pick` непустая `edit`-полоса (а после неё `topic`) всегда идёт раньше готового обычного сообщения. Если правки приходят быстрее, чем их обслуживает один запрос в полёте, `sendMessage` не уходит, пока поток не кончится. Проверено scratch-тестом `crev_sustained_edits_starve_messages`: transport с задержкой 200 мс, правки разных сообщений каждые 100 мс в течение 60 с. Сообщение, поставленное в t=0 при полном bucket, ушло только в **120.6 с**. Коалесинг по `message_id` ограничивает очередь числом разных редактируемых сообщений, так что в реальном трафике это маловероятно. Но будущий "живой прогресс" по многим сообщениям (TASK-009/011) может это вызвать. Предложение: когда message-токен готов, чередовать полосы (например, не больше N unmetered подряд, пока готово metered-сообщение), или хотя бы задокументировать инвариант для вызывающих.

### minor-2: `retry_after = 0` приводит к повторам без паузы
`crates/cctg/src/hub/api.rs:365` и `scheduler.rs:413`. `parameters.retry_after: 0` превращается в `Duration::ZERO`, пауза заканчивается сразу. Для unmetered-полос токенов нет, и ничего не ограничивает частоту. Scratch-тест `crev_retry_after_zero_on_edit`: 51 попытка за 0 нс (fake отвечал 50 раз 429 с `retry_after: 0`). Telegram обычно отдаёт ≥ 1 с, но это внешний ввод. Предложение: `max(retry_after, 1 s)` в `parse_envelope`.

### minor-3: `.env` пишется в env процесса hub
`crates/cctg/src/hub/config.rs:143`. `dotenvy::from_path` вызывает `set_var`, и `CCTG_BOT_TOKEN` наследует каждый дочерний процесс hub. Сейчас их нет. Если hub на главном устройстве когда-нибудь запустит `claude -p --resume` или шелл сам, токен окажется в env чужого процесса, где его может увидеть tool Bash. К этому добавляется гонка `set_var` под Unix, которую уже отметил PLAN_FINAL. Предложение (отдельной мелкой задачей): `dotenvy::from_path_iter` → карта значений → `Config::from_vars(|k| env::var(k).ok().or_else(|| map.get(k).cloned()))`, без `set_var`.

## 4. Missing coverage

- Нет теста на скользящее окно при произвольных моментах поступления и 429 в середине потока. Существующие тесты ставят всё в очередь в t=0. Мой `crev_random_arrivals_and_429_keep_window` проходит, его стоит перенести в набор.
- Нет теста на честность между `edit`/`topic` и `message` (см. minor-1).
- Нет теста на `retry_after: 0` (см. minor-2).
- `poll` не покрыт тестом целиком: смена offset между батчами, backoff и 429 проверяются только чтением кода (`updates.rs:169-191`). Вынести шаг цикла в чистую функцию дёшево, но это не блокер.
- Нет теста на callback без `message` (inline-режим): такой проходит проверку чата (`updates.rs:115-121`) и держится только на allowlist. Поведение верное, фиксации тестом нет.

## 5. Nits

- `updates.rs:150-156`: если во всём батче ни у одного апдейта нет целого `update_id`, offset не двигается, а `backoff` сбрасывается на `Ok`. `getUpdates` тогда будет крутиться без паузы. От Telegram такого не бывает, отмечаю для полноты.
- `scheduler.rs:286-288`: `try_recv` сливает mailbox в неограниченные `VecDeque`. Реальная граница здесь только backpressure на `submit` во время `dispatch`. PLAN_FINAL это принял.
- `api.rs:173`: `chat_id()` публичный только ради `poll`. Нормально, отмечаю как единственного потребителя.
- Процесс: implementer собирал release в repo `target/` (`scratch/release_measure_implementer.out.txt: binary=...\cctg\target\release\cctg.exe`), хотя план требовал target dir вне репо. `target/` в gitignore, вреда нет.

## Evidence

- Scratch-копия с adversarial-тестами: `maw/tasks/in_progress/TASK-008/scratch/crev/ws/crates/cctg/src/hub/scheduler.rs` (тесты `crev_*`), запуск `cargo test -p cctg --lib --offline crev -- --nocapture`. Вывод: `CREV attempts 210 max in closed 60s window 20`, `CREV message sent at Some(120.6s)`, `CREV retry_after=0: 51 attempts, last at Some(0ns)`.
- Dead-end записи implementer в `log.jsonl` касаются только скрипта флейк-проверки (`run_flake_checks.ps1`). Продуктового кода они не затрагивают. Независимый прогон на флейки выше: 0 падений.

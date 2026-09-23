# PLAN FINAL — TASK-008: hub — Telegram Bot API client and outbound scheduler

Stage: plan-reviewer-2 (claude/opus, effort=medium). Пути относительно корня репозитория `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-008`. `REF` = `T/scratch/reviewer2/ws`: копия reference планировщика (`T/scratch/planner/ws`) с тремя исправлениями этого ревью. REF собран и прогнан целиком (fmt, clippy `-D warnings`, `cargo test --workspace`, 12 мутаций, прогон на флейки). Реальный Telegram не вызывался, `.env` не читался.

## 1. Summary

В `crates/cctg` появляется lib-часть с модулем `hub`: тонкий Bot API клиент на голом `reqwest` 0.13 (rustls) с узкими `#[serde(default)]` типами и 11 методами (`api.rs`), загрузка конфига из `.env` и process env с редакцией секретов (`config.rs`), толерантный long polling с allowlist по `from.id` и распознаванием служебных `forum_topic_*` (`updates.rs`), и один outbound-планировщик (`scheduler.rs`). Планировщик: token bucket на группу (ёмкость 5, 1 токен в 4 с, минимум 1 с между отправками, значит не больше 20 в любом 60-секундном окне), одна глобальная FIFO-очередь для `sendMessage`/`sendDocument`, permission-промпт обгоняет только чужие темы и никогда не обгоняет более старое сообщение своей темы, коалесинг правок одного `message_id`, отдельные неметрированные полосы для правок и мутаций тем, любой 429 ставит на паузу всю очередь до `retry_after` ровно с одной повторной попыткой. `cctg hub [--env-file PATH]` на старте делает `getMe` → `getChatMember` → проверку `can_manage_topics` и только потом запускает планировщик и поллинг. Реализация есть файл в файл в REF. Исполнитель копирует 12 файлов, сверяет SHA-256, прогоняет проверки и пишет `T/IMPL_SUMMARY.md` с измерениями.

## 2. Implementation steps

### Step 0. Изоляция и baseline

Только PowerShell или Git Bash; cargo-команды по одной (памяти на хосте мало). Target dir вне репозитория:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task008-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание на текущем HEAD: всё зелёное, **73 теста** (cctg: 1 unit в `main.rs` + 1 в `tests/stdout.rs`; transcript: 70 + 1 doc-test). Проверить, что реализационные файлы не тронуты: `git status --short -- Cargo.toml Cargo.lock crates` пусто. Если нет, остановиться и сообщить, чужие изменения не затирать. `.env` не открывать, реальные Telegram-вызовы не делать, `.claude/` не трогать.

### Step 1. Скопировать 12 файлов из REF

Скопировать каждый `REF/<path>` в `<path>` байт в байт (UTF-8 без BOM, LF; `core.autocrlf=true` в репо нормализует при коммите, это нормально). Затем из корня репозитория сверить хэши:

```bash
sha256sum -c maw/tasks/in_progress/TASK-008/scratch/reviewer2/hashes.txt
```

(Тот же файл проходит `sha256sum -c` и из `REF`.) Все 12 строк должны быть `OK`.

| Path | Вид | SHA-256 | Что внутри |
|---|---|---|---|
| `Cargo.toml` | edit | `c431c758f33f38e2a86dee30466f2aa2a1bcdec0586b4bcc7d91f9953a5d8b96` | + workspace deps `dotenvy = "0.15"`, `reqwest = { version = "0.13", default-features = false, features = ["json", "multipart", "rustls"] }`, `thiserror = "2"` |
| `Cargo.lock` | regenerated | `6cce2b5eb6aa0aced15b69a5c5016f098a3102d516bbdcafa87d4ab9cc9eddc7` | 188 пакетов; все уже в локальном `~/.cargo/registry`, сборка `--offline` |
| `crates/cctg/Cargo.toml` | edit | `e112906b03a5d093468ce5dbe2341302e89dd31385368ca99e0399f79868b26f` | + `dotenvy, reqwest, serde, serde_json, thiserror`; tokio `+ ["sync", "time"]`; dev-dep tokio `["test-util"]` |
| `crates/cctg/src/main.rs` | edit | `d7d41364e639477102a58ebe69804c3c438b52804251fd201d30bb78d1894d17` | `Hub { #[arg(long)] env_file: Option<PathBuf> }` → `cctg::hub::run(env_file.as_deref()).await?`; unit-тест парсит `hub` и `hub --env-file x.env` |
| `crates/cctg/src/lib.rs` | new | `4f7bf56a27eec147c94eb0bfb24630549281743f8970e91a3d25c0565c176761` | `pub mod hub;` |
| `crates/cctg/src/hub/mod.rs` | new | `ee1078d93e35a4273202b688eb8d9c7ea76b466419ee245c19622b803b8849e2` | `run`, `RightsError`, `check_topic_rights`, порядок старта |
| `crates/cctg/src/hub/api.rs` | new | `3bf39a963b984f0f739325a9ea530547cfe635ea4757fc761080888033de527d` | `BotApi`, envelope, 11 методов, `ApiError::{Http,RetryAfter,Telegram,Decode}`, `without_url()` на каждом `reqwest::Error` |
| `crates/cctg/src/hub/config.rs` | new | `3586625507a0091dc9dd07d3c099da65874a17c3f2ec004432d0c5312d291194` | **исправлен**: `Config::load -> Result<Self, ConfigError>`, `ConfigError::EnvFile { path, reason }` без `dotenvy::Error` внутри |
| `crates/cctg/src/hub/scheduler.rs` | new | `57b8867d40bf930d6a925121c008fd52ff3b1f2ec015fe3489a773efc4ff9830` | **исправлен**: permission-промпты живут в общей `Message`-очереди, выбираются через `next_permission()` |
| `crates/cctg/src/hub/updates.rs` | new | `751b60cbca2990f7f0ae117343e9205a16065a29a8f60de54f895b577638934d` | **изменён**: флейки-тест захвата логов вынесен |
| `crates/cctg/tests/stdout.rs` | edit | `a788e6f6ed52a31f9afe2940ef2333d461dc7d3f5de759a2b46a8e00c5ed37f8` | `agent`/`hook` молчат; `hub` без конфига падает; **новый** тест битого `.env` |
| `crates/cctg/tests/routing_logs.rs` | new | `bdf472319c94dd39eec1778349c5f91a7fb4f7899db65f4a0401aaf618f511c6` | **новый**: захват TRACE-логов роутинга в отдельном test binary |

Точный diff исправлений относительно reference планировщика: `T/scratch/reviewer2/fix.diff`. Ниже по каждому файлу, что он делает и почему. Ничего не переписывать вручную: если хэш не совпал, скопировать заново.

**1a. Manifests и wiring.** `crates/cctg` становится lib+bin. Причина: API-поверхность (`sendDocument`, `createForumTopic`, `Outbox`) потребляется только в TASK-009/011; в bin-only крейте это dead code, и `clippy -D warnings` падает. Прямые зависимости `cctg` ровно: `anyhow, clap, dotenvy, reqwest, serde, serde_json, thiserror, tokio, tracing, tracing-subscriber`.

**1b. `config.rs`.** Переменные `CCTG_BOT_TOKEN` (`<digits>:<secret>`), `CCTG_CHAT_ID` (строго `-100<digits>`), `CCTG_ALLOWED_USER_IDS` (непустой список чисел через запятую). `Config::load(Some(path))` читает только этот файл, `Config::load(None)` только `./.env`, если он есть (`dotenvy::dotenv()` не используется: он ищет файл вверх по родителям). Process env побеждает файл. `BotToken`, `Allowlist` с ручным `Debug`; ошибки называют переменную или номер элемента, но не значение. Исправление: любая ошибка `dotenvy::from_path` превращается в `ConfigError::EnvFile { path, reason }` с `reason` из фиксированного набора (`"a line cannot be parsed, check quotes"`, `"file not found"`, `"the file cannot be read"`); сам `dotenvy::Error` не хранится и не становится `source`. Причина: `dotenvy::Error::LineParse` в Display цитирует строку и весь остаток файла, reference через `anyhow::Context` выводил токен и allowlist id в stderr (воспроизведено, см. Test plan).

**1c. `api.rs`** без изменений против reference. Токен только в `BotApi.base`, `Debug` ручной. Каждый `reqwest::Error` уходит через `ApiError::http` → `without_url()`; тело читается `.bytes()` и разбирается `parse_envelope` вручную (никаких `Response::json`/`error_for_status`, они держат URL). 429 → `RetryAfter(parameters.retry_after)`, без `parameters` → запасные 5 с (это ответ на 429, а не лимит мутаций). `getUpdates` возвращает `Vec<Value>`, `allowed_updates = ["message","callback_query"]`, long poll 50 с, HTTP timeout 65 с.

**1d. `updates.rs`.** `classify`: чужой чат → `OtherChat`; служебные `forum_topic_created/edited/closed/reopened` распознаются до allowlist (их шлёт бот) и возвращаются как `Routed::Service{kind, message_id, thread_id}`, никогда как `Input`; потом allowlist по `from.id` для message и callback. `Inbound`/`CallbackInput` не содержат user id. `route_batch` разбирает каждый элемент отдельно, битый/неизвестный даёт `Ignored`, offset двигается по любому целому `update_id`. `poll` бесконечный: 429 ждёт `retry_after`, прочее backoff 1→30 с. Изменение против reference: тест `routing_logs_never_contain_user_ids` и его `Captured` writer удалены из unit-тестов (перенесены в `tests/routing_logs.rs`, см. 1g).

**1e. `scheduler.rs`.** Один actor, один запрос в полёте, bounded mailbox 1024. Очереди: `edit` (`editMessageText` с коалесингом по `message_id` на месте старой правки, вытесненный waiter получает `Outcome::Superseded`, плюс `answerCallbackQuery`), `topic` (`createForumTopic`, `editForumTopic`, `deleteMessage`), `message` (`sendMessage`, `sendDocument`, включая permission-промпты). Порядок выбора в `pick`:
1. если очередь на паузе после 429, ждать `paused_until`;
2. `next_permission()`: первый в `message` permission-`Send`, у темы (`thread_id`, `None` = General) которого нет более раннего задания в `message`; если такой есть и bucket готов, отправить его;
3. голова `edit`; 4. голова `topic` (обе без токена и без численного лимита);
5. иначе голова `message`, когда bucket готов (иначе `Pick::At(ready)`).

`dispatch` делает `remove(index)`, берёт токен для metered до вызова (неудачная попытка тоже тратит токен), на `RetryAfter` ставит паузу на всю очередь и кладёт задание в голову его очереди (для промпта из середины это корректно: более старых заданий его темы нет). Исправление против reference: полоса `Permission` удалена. В reference промпт всегда шёл раньше `Message` без учёта темы, и сообщение A темы 7, поставленное раньше промпта B темы 7, уходило после B.

**1f. `mod.rs`** без изменений: `Config::load` → `BotApi::new` → `getMe` → `getChatMember(bot id)` → `check_topic_rights` (`creator` ок, `administrator` обязан иметь `can_manage_topics`, прочее `NotAdmin(status)`, тексты называют `can_manage_topics`/"Manage Topics") → warning при отсутствии `can_delete_messages` → spawn scheduler → `updates::poll`. Логируется только `@username` бота и `thread_id`.

**1g. Тесты-файлы.** `tests/stdout.rs`: `agent` и `hook SessionStart` успешны и молчат в stdout; `hub_without_config_fails_on_stderr_only` (пустой cwd, `CCTG_*` удалены, stderr содержит `CCTG_BOT_TOKEN is not set`); новый `malformed_env_file_does_not_echo_its_contents`. `tests/routing_logs.rs`: единственный тест в своём binary, потому что `tracing` кэширует interest callsite глобально, и callsite, впервые задетый параллельным тестом до создания scoped subscriber, остаётся выключенным.

### Step 2. Проверки

По одной команде, `CARGO_TARGET_DIR` как в Step 0:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
cargo tree -p cctg --edges normal --depth 1 --offline
git status --short
```

Ожидание (прогон REF: `T/scratch/reviewer2/verify.out.txt`):
- fmt и clippy чистые.
- **103 теста**: cctg lib 27, cctg main 1, `tests/routing_logs.rs` 1, `tests/stdout.rs` 3; transcript 10+15+3+14+14+14 = 70 плюс 1 doc-test. Transcript-наборы совпадают с baseline.
- `cargo tree`: ровно 10 прямых зависимостей из Step 1a.
- `git status`: 12 файлов из таблицы, плюс артефакты задачи; `target/` в репо не появляется.

Проверка на флейки (обязательна, reference падал в 56 из 100 прогонов): найти lib test binary через `cargo test -p cctg --lib --offline --no-run` (строка `Executable unittests src\lib.rs (...)`) и запустить его 50 раз подряд с `-q`; должно быть 0 падений. Так же 50 раз `routing_logs-*.exe`. Результат записать в `IMPL_SUMMARY.md`.

### Step 3. `T/IMPL_SUMMARY.md` с измерениями (acceptance criterion 8)

Создать файл и перенести таблицу. Исполнитель пересобирает только release-размер (одна сборка, можно в тот же target dir): `cargo build --release -p cctg --offline`, затем размер `%CARGO_TARGET_DIR%\release\cctg.exe`. Clean build и RSS не перемерять: RSS-проба ходит в реальный Telegram и читает `.env`, этого в песочнице делать нельзя.

| Metric | HEAD без hub (no-op) | TASK-008 | Evidence |
|---|---:|---:|---|
| Release `cctg.exe` | 994,304 B | 5,490,688 B (REF финальный); planner reference 5,485,056 B | `T/scratch/reviewer2/release_measure.out.txt`, planner `ls -l` |
| Clean release build, `cargo build --release -p cctg --offline`, пустой target | 8.4 s, 40 crates | 45.6 s, 127 crates (planner, простаивающий хост); 91 s на нагруженном хосте (reviewer-2) | `T/scratch/planner/build_base.log`, `build_ref.log`, `T/scratch/reviewer2/release_build.log` |
| Idle память после TLS-вызовов старта, scheduler запущен, без `getUpdates` | n/a | working set ≈18.7 MB, private ≈5.3 MB, ровно 5-40 s | `T/scratch/planner/rss_probe.out.txt` |

Хост: 16 потоков, rustc 1.95.0, Windows 11. Доминирует `aws-lc-sys` (C toolchain). RSS под живым long poll по решению оркестратора меряется в TASK-009. Добавить строку с размером, который получил сам исполнитель. Сюда же список тестов и результат прогона на флейки.

### Step 4. Коммит

Ветка `feature/hub-telegram-foundation`. В коммит продукта: 12 файлов из таблицы. `T/IMPL_SUMMARY.md` отдельно или тем же коммитом, по правилам pipeline. Не коммитить `.env`, `.claude/`, `registry.json`, `target/`, probe-исходники. Сообщение на английском, без "Generated with" и "Co-Authored-By" (закон проекта). Пример: `hub: Bot API client, config, tolerant polling and outbound scheduler`.

## 3. Test plan

Все тесты уже в файлах REF, отдельно писать ничего не нужно. Время подменено (`#[tokio::test(start_paused = true)]`), сеть только loopback.

| Тест | Критерий | Что доказывает |
|---|---|---|
| `updates::allowlisted_text_is_input_and_strangers_are_dropped` | 1 | message и callback от `from.id` вне allowlist дают `Ignored::NotAllowed`, от allowlisted `Input`/`Callback` |
| `updates::other_chats_and_senderless_messages_are_ignored` | 1 | чужой чат `OtherChat`, без `from` `NoSender` |
| `tests/routing_logs.rs::routing_logs_never_contain_user_ids` | 1 | TRACE-логи батча со stranger/allowed/bot/битым апдейтом содержат `update ignored` и `forum service message`, но ни одного id. Детерминирован (0/200 падений) |
| `api::transport_errors_never_contain_the_token` | 1 | реальная ошибка reqwest (loopback порт 9): Display, Debug, anyhow `{:#}`, `BotApi` Debug без секрета и без `777:` |
| `config::debug_hides_token_and_user_ids`, `config::missing_and_bad_values_are_named_but_not_echoed` | 1 | Debug и ошибки конфига не повторяют значения |
| `config::missing_env_file_is_named` | 1 | отсутствующий `--env-file` даёт `EnvFile { path, reason: "file not found" }` |
| `stdout::malformed_env_file_does_not_echo_its_contents` | 1 | `cctg hub --env-file bad.env` с незакрытой кавычкой: exit != 0, stdout пуст, stderr называет файл, но не содержит synthetic-токен, synthetic user id, `777:` и `CCTG_ALLOWED_USER_IDS=` |
| `scheduler::group_limit_and_topic_order_hold` | 2 | 60 отправок в 3 темы: любое окно 60 s ≤ 20, промежуток ≥ 1 s, порядок каждой темы 0..19, успевает к 225 s |
| `scheduler::default_bucket_fits_twenty_per_minute` | 2 | `capacity + 60/refill ≤ 20`, `min_gap ≥ 1 s` |
| `scheduler::failed_attempts_spend_tokens` | 2, 3 | пять 429 по 1 s и 26 отправок: в любом окне ≤ 20 попыток, включая неудачные |
| `scheduler::permission_never_overtakes_its_own_topic` | 2 | send A, document D, permission P в теме 7 → A, D, P |
| `scheduler::mixed_chain_in_one_topic_keeps_enqueue_order` | 2 | P1, O, P2 в теме 5, потом Z в теме 6 → P1, O, P2, Z |
| `scheduler::permission_overtakes_other_topics_only` | 2 + приоритет | m0(t1), x(t2), p2(t2), m1(t1), m2(t1), p3(t3) → p3, m0, x, p2, m1, m2 |
| `scheduler::permission_prompt_jumps_the_queue` | приоритет | промпт в теме 2 после 10 сообщений темы 1 уходит первым |
| `scheduler::retried_permission_keeps_topic_order` | 2, 3 | m0(t1), p(t2), after(t2), 429 на p: p@0, p@3, m0@4, after@5 |
| `scheduler::repeated_edits_of_one_message_coalesce` | 3 | правки 7:a, 8:x, 7:b, 7:c → отправлены `c`, `x`; первые два waiter `Superseded` |
| `scheduler::retry_after_pauses_everything_and_retries_once` | 3 | 429 (7 s): до 7 s тишина во всех полосах, потом одна повторная попытка |
| `scheduler::repeated_429_is_one_attempt_per_retry_after` | 3 | три 429 по 3 s: попытки ровно в 0, 3, 6, 9 s, следующее сообщение в 10 s |
| `api::maps_429_to_retry_after` | 3 | `parameters.retry_after` → 7 s; без него → 5 s |
| `scheduler::topic_mutations_and_edits_do_not_spend_message_tokens` | 4 | 40 create/edit topic/delete/edit уходят в t=0, после них полный burst 5 сообщений в 0..4 s |
| `updates::forum_service_messages_are_never_input` | 5 | все четыре `forum_topic_*` от бота и от allowlisted админа → `Routed::Service` |
| `updates::unknown_types_fields_and_bad_shapes_do_not_stop_the_batch` | 6 | неизвестный тип, лишние поля, `text: 5`, без `update_id`, не-объект: батч продолжается, offset 9 |
| `api::decodes_ok_result_and_ignores_unknown_fields` | 6, 7 | реальный набор ключей `getChatMember` декодируется в 3 поля |
| `hub::tests::missing_manage_topics_is_a_startup_error` | 7 | матрица статусов; текст ошибки содержит `can_manage_topics` |
| `stdout::hub_without_config_fails_on_stderr_only` | 7, 9 | hub без конфига падает на старте, stdout пуст |
| `api::maps_other_errors_with_description`, `config::reads_a_complete_config`, `config::chat_id_must_use_the_bot_api_form`, `scheduler::stops_after_outbox_is_dropped_and_queue_drained`, `main::parses_all_subcommands`, `stdout::subcommands_do_not_write_to_stdout` | support | маппинг ошибок, форма `-100`, чистое завершение, CLI |
| весь `transcript` | 9 | 71 тест без изменений |

Критерий 4 "лимит мутаций не захардкожен": в `scheduler.rs` нет константы или конфига для `edit`/`topic`; `BucketConfig` используется только для `Op::metered()`. Исполнитель проверяет это глазами в diff.

Мутационное покрытие REF (`T/scratch/reviewer2/mutations.out.txt`, скрипт `mutate.py`): 12 из 12 убиты. Шесть мутаций планировщика (без `without_url`, capacity 20, всё кроме правок метрировано, без паузы 429, без allowlist, стоп батча на битом апдейте) и шесть на исправления (промпт без учёта темы, без приоритета промптов, 429-задание в хвост, токен только за успех, текст `dotenvy` в `EnvFile`, лог сырого апдейта). Исполнителю повторять мутации не нужно.

Воспроизведение дефектов на неизменённом reference: `T/scratch/reviewer2/repro.out.txt` (промпт ушёл раньше: `["prompt","ordinary","doc"]`; stderr содержал synthetic-токен и id), `T/scratch/reviewer2/flake.out.txt` (56/100 и 45/50 падений `routing_logs...` в reference, 0/100 lib и 0/200 `routing_logs` после исправления).

## 4. Rollout notes

- **Новая обязательная переменная `CCTG_ALLOWED_USER_IDS`** (решение оркестратора). Пользователь добавляет свой Telegram user id в `.env` до первого живого `cctg hub`. Без неё hub не стартует с ошибкой, которая называет переменную. Значения в логи, тесты и коммиты не попадают.
- **`cctg hub` больше не no-op.** Без конфига он падает с ненулевым кодом. `agent` и `hook` по-прежнему no-op и молчат в stdout.
- **Первый живой запуск** (TASK-009 или вручную) подтвердит 2 висящих апдейта бота; они старые, их никто не ждёт.
- **Toolchain.** `reqwest 0.13` + `rustls` тянет `aws-lc-sys`, нужен C toolchain (MSVC есть на этом хосте). Для нового устройства или CI нужно то же. Запасной путь (`rustls-no-provider` + ring) только отдельной задачей с новой сборкой и измерениями.
- **Offline сборка.** Lock копируется как есть, версии не менять. Если у песочницы нет доступа на чтение к `~/.cargo/registry`, остановиться и сообщить.
- **Миграций, feature flags, изменений формата нет.** `registry.json` в этой задаче не создаётся.
- **Устаревшие cargo-артефакты.** Если файл восстанавливался с более старым mtime, cargo может не пересобрать. После любого восстановления делать `touch` исходников.

## 5. Review notes

### Контрпример, проверенный первым

"Обычное сообщение A в теме 7, потом permission B в той же теме; планировщик отправляет B раньше A." **Подтвердился** на неизменённом reference (`permission_never_overtakes_its_own_topic` получил `["prompt","ordinary","doc"]`). Второй контрпример, битый `.env`, выводящий токен в stderr, тоже **подтвердился**. Запись: `T/scratch/reviewer2/disconfirmation.md`.

### Что изменено относительно PLAN_V2 и почему

1. **Исправления выполнены и проверены, а не только описаны.** PLAN_V2 говорил, что хэши устарели, и предлагал сверять "по поведению". Теперь есть собранный и протестированный REF с новыми хэшами, исполнитель копирует файл в файл.
2. **Другая форма FIFO-фикса.** PLAN_V2 предлагал очередь на каждую тему плюс sequence number. Я оставил одну `message`-очередь (она и так глобальная FIFO по решению оркестратора) и добавил `next_permission()`: промпт выбирается, только если в очереди нет более раннего задания его темы. Семантика та же ("обгон только между темами"), но нет второй структуры данных и sequence-счётчика. Документы (`sendDocument`) тоже участвуют в порядке темы, это покрыто тестом. Обоснование в `T/log.jsonl`.
3. **Найден третий дефект, которого не было в PLAN_V2: флейки-тест.** `updates::tests::routing_logs_never_contain_user_ids` в reference падает в 56 из 100 прогонов lib binary (45/50 при фильтре `hub::updates`). Причина: гонка регистрации callsite `tracing` между параллельными тестами; callsite остаётся с кэшированным interest "never", и событие не доходит до scoped subscriber. Планировщику и reviewer-1 просто повезло. Это прямо ломает критерий "Existing tests pass". Тест перенесён в свой test binary, после этого 0/200. Вариант с `rebuild_interest_cache()` отвергнут: он сужает гонку, но не убирает.
4. **Добавлен `failed_attempts_spend_tokens`.** PLAN_V2 требовал проверку окна 20/мин после 429 с учётом неудачных попыток, но теста не было. Он убивает мутацию "токен только за успех".
5. **Санитизация `.env` сделана типом, а не контекстом.** `Config::load` возвращает `ConfigError`, `dotenvy::Error` не хранится; `reason` из трёх фиксированных строк. Покрыто subprocess-тестом по реальному stderr CLI, как требовал PLAN_V2, и unit-тестом на отсутствующий файл.
6. **Число тестов:** 103 (было 96 в reference, PLAN_V2 не называл итог). Baseline 73 подтверждён.
7. **Убрано из PLAN_V2 как непроверяемое:** ограниченный drain mailbox. Голодания воспроизвести нельзя, mailbox ограничен 1024, продюсеры ограничены трафиком Telegram и агентов. Reference оставлен как есть.
8. **Измерения обновлены:** финальный release 5,490,688 B; clean build на нагруженном хосте 91 s против 45.6 s у планировщика на простаивающем. В заметки идут оба числа с условиями.

### Охота за другими путями утечки (результат: новых утечек нет)

- `reqwest` и `hyper-util` на уровнях INFO/WARN не логируют URL (проверено grep по исходникам 0.13.5 / 0.1.20 в `~/.cargo/registry`). `tracing_subscriber::fmt()` по умолчанию INFO (`DEFAULT_MAX_LEVEL`), feature `env-filter` не включена, так что `RUST_LOG` не может включить DEBUG/TRACE зависимостей. Логи `log`-крейта через tracing-log тоже ограничены INFO.
- `ApiError::Decode(serde_json::Error)` может процитировать значение неверного типа. Строго декодируются только ответы про самого бота, чат и наш собственный текст; апдейты с user id идут через `route_batch`, где ошибка выбрасывается. До `from.id` и секрета токена этот путь не доходит, поэтому не менял.
- `ApiError::Telegram{description}` содержит внешний текст Telegram; токен и user id в тела запросов не кладутся (только `getChatMember(bot id)`). Если будущий метод добавит user id в запрос, контракт ошибок надо пересмотреть.
- Невалидный URL из-за странного токена: `reqwest` кладёт URL в поле `url`, `without_url()` его снимает; `url::ParseError` вход не цитирует.
- В не-тестовом коде нет `unwrap`/`expect`/`println!`/`dbg!` на внешнем вводе (grep).

### Замечания без изменений кода

- `dotenvy::from_path` делает `std::env::set_var` уже внутри tokio multi-thread runtime. На Windows безопасно, на Unix это гонка с чтением env из других потоков, но на старте никто env не читает. Если hub поедет на Linux, стоит перейти на `dotenvy::from_path_iter` и передавать значения в `Config::from_vars` без `set_var`.
- Правки и мутации тем обходят `message`-очередь, поэтому `EditTopic`/`Delete` могут уйти раньше ранее поставленного `Send` в той же теме. Критерий FIFO касается сообщений, будущим вызывающим сторонам это надо учитывать.
- Предложение для project context (сам его не пишу, у этой роли нет прав на запись): в hub/transcript risk lessons добавить "тесты, которые захватывают `tracing` через `with_default`, держать в отдельном integration test binary: interest callsite кэшируется глобально, и параллельный тест делает такой захват флейки".

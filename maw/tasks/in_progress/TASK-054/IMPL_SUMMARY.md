# TASK-054 IMPL_SUMMARY

Commit `1ccecd6` on `feature/outbound-debounce`.

## 1. Что сделано

Файлы:
- `crates/cctg/src/hub/scheduler.rs`: +477 / -31 (из них ~330 строк это тесты)
- `crates/cctg/src/hub/mod.rs`: +3 / -3 (hub берёт `Limits::default()`; импорт `BucketConfig` переехал в тесты)

Устройство:
- `Limits { messages, edits: Option<BucketConfig>, debounce, debounce_max }`. `Limits::default()` это настройки hub: сообщения `BucketConfig::default()` (20/мин, зазор 1 с), правки `EDIT_BUCKET` (5 + 1 раз в 4 с = 20/мин, без зазора), `DEBOUNCE` 1.5 с, `DEBOUNCE_MAX` 4 с. Каждое число обосновано в doc-комментарии константы.
- `Scheduler::new(transport, impl Into<Limits>)`. `From<BucketConfig>` даёт старое поведение (без дебаунса, правки без лимита), поэтому ~25 тестовых вызовов с быстрыми bucket'ами не менялись.
- Дебаунс (`due`, `next_message`): у `Job` есть `queued_at` (время submit). Строка `merge`, первая в своей теме, ждёт `debounce` тишины после последней строки, которая к ней приклеится, но не дольше `debounce_max` от своего прихода. Если в очереди темы стоит сообщение, которое приклеиться не может (ответ, строка `merge: false`, громкая строка после тихих), ожидание кончается сразу. Промпт той же темы ожидание не прерывает: он всё равно уходит раньше. Ждущая строка держит только свою тему, остальные темы идут. При включённом дебаунсе `merge_lines` склеивает всегда, а не только когда не хватает токенов. Строки после plain-retry и строки после 429 не ждут повторно.
- Бюджет правок: `Edit`, `React`, `Delete`, `Pin`, `CreateTopic`, `EditTopic` берут токен из `edit_bucket` (`Op::edit_metered`). `AnswerCallback` токен не берёт и, пока bucket пуст, может обогнать ждущие правки (`Lane::Edit(index)`, `next_edit`). Сообщения и правки берут токены из разных bucket'ов, поэтому поток статусов не отнимает бюджет у сообщений, и наоборот. Правило «сообщение после 4 unmetered» осталось.
- Doc модуля обновлён: дебаунс, второй bucket, callback-исключение.

## 2. Отклонения и открытые моменты

- Числа для правок это обоснованная догадка: Telegram не публикует лимит на правки. Сумма запросов в группу теперь не больше 40/мин (20 сообщений и 20 правок). Если 429 останутся, крутить `EDIT_BUCKET`.
- «Статус не чаще раза в несколько секунд» уже обеспечивал `slots::STATUS_EVERY` = 5 с на слот, код не менялся. Суммарно статусы теперь режет `EDIT_BUCKET`; правки одного сообщения по-прежнему коалесцируются, так что при задержке уходит самый свежий текст.
- Существующие e2e-тесты передают `BucketConfig`, то есть идут без дебаунса и без бюджета правок. Ни один e2e-тест не менялся. Новое поведение покрыто юнит-тестами scheduler на paused time.
- `tests/soak.rs` в live-режиме по-прежнему использует `BucketConfig::default()`, то есть без дебаунса. Не трогал (хирургично); если нужен live soak с настройками hub, туда надо подставить `Limits::default()`. Soak читает поля bucket для своих отчётов, так что это отдельная правка.
- Предложение по project context (инвариант flood control в domain hub): `PCTX_PROPOSALS.md`.

## 3. Тесты

Все сборки с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, перед запуском тестов сделан touch `lib.rs`/`main.rs`.
- `cargo fmt --all -- --check`: ok
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok
- `cargo test -j 1 --workspace`: exit 0; lib 737 passed / 1 ignored, все интеграционные бинарники ok, падений и flaky нет. Лог: `scratch/test-full.log`.

Новые тесты в `hub::scheduler::tests`:
- `a_burst_of_lines_goes_as_one_message_with_budget_to_spare`: строки в 0 / 0.5 / 1.0 с уходят одним сообщением в 2.5 с (Sent, Merged, Merged)
- `a_lone_line_goes_after_the_quiet_window`: одиночная строка уходит ровно через 1.5 с
- `a_steady_trickle_of_lines_still_shows_every_few_seconds`: строки каждую 1 с, первое сообщение через 4 с, всё по разу и по порядку
- `a_permission_prompt_does_not_wait_for_the_debounce`: промпт уходит сразу, строки после него
- `a_waiting_line_holds_back_only_its_own_topic`: другая тема не ждёт; сообщение той же темы, которое не приклеится, кончает ожидание
- `an_answer_queued_after_the_lines_ends_their_wait`
- `a_debounced_message_refused_with_429_goes_again_after_the_pause`
- `edits_reactions_and_topic_mutations_stay_within_the_edit_budget`: 160 операций; в любом окне 60 с не больше 20 правок, 20 отправок и 40 запросов; первые 5 сообщений в 0..4 с, правки их не держат
- `callback_answers_do_not_wait_for_the_edit_budget`
- `default_edit_budget_fits_twenty_per_minute`

## 4. Как проверить руками

- `cargo test -p cctg --lib hub::scheduler`
- Вживую: hub с несколькими активными сессиями. Строки инструментов одной темы приходят пачками раз в 1.5-4 с одним сообщением, промпты разрешений приходят сразу. В логе hub должно стать заметно меньше `telegram flood control, outbound queue paused`.

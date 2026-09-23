# QA_REPORT — TASK-009: hub, local transcript commands

Stage: qa (claude/opus, effort=medium). Ветка `feature/hub-transcript-commands`, HEAD `0268938`. Код: `git diff main -- Cargo.toml Cargo.lock crates/`.

## 0. Disconfirmation

Контрпример, который проверялся первым: "фикс #1 (notice о неудачной доставке) зацикливается или глушит worker. Если Telegram постоянно отвечает не-too-long ошибкой, notice тоже падает, и тогда либо идёт новый notice, либо следующая команда не обрабатывается. Второй вариант: остановленный scheduler вешает worker".

Результат: **не подтвердился.** `persistent_failure_gives_one_notice_per_command_no_loop`: на три команды при постоянной ошибке 400 ровно 6 ops, `[Send, NOTICE] × 3`, повторов нет, Usage-ветка ведёт себя так же. `failure_then_recovery_next_command_works`: после сбоя следующая команда `/full` отвечает ровно выводом библиотеки. `stopped_scheduler_does_not_hang_worker`: при остановленном scheduler worker завершается, в 10-секундный таймаут укладывается.

`log.jsonl` до QA не содержал `dead_end`. Решения планировщика и reviewer-2 (сохранение до обработки, next offset ниже старого, 24 ч, 256 MiB, без заголовка) сверены с кодом `updates.rs`/`offset.rs`/`commands.rs`, все на месте.

## 1. Environment

- docker-compose, dev-target и live-сервисов нет. Использовались `cargo test` на рабочем дереве и отдельный scratch-крейт `scratch/qa` (path-зависимости на `crates/cctg`, `crates/transcript`), `[workspace]` свой.
- `CARGO_TARGET_DIR`: `%TEMP%/cctg-qa009-target` (workspace), `%TEMP%/cctg-qa009-probe-target` (scratch). В репозитории target не создавался. Сборки шли по одной.
- Telegram не вызывался, `.env` не читался. Реальный `~/.claude/projects` читался только через read-only probe, который печатает счётчики, булевы значения и тайминги.
- Сервисов не поднималось, чистить нечего.

Воспроизведение:
```bash
export CARGO_TARGET_DIR="$TEMP/cctg-qa009-target"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
cd maw/tasks/in_progress/TASK-009/scratch/qa
export CARGO_TARGET_DIR="$TEMP/cctg-qa009-probe-target"
cargo test --offline --test qa && cargo test --offline --test logs
cargo run --release --offline     # read-only probe over ~/.claude/projects
```

## 2. Test results

**Существующий набор:**
- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: exit 0, предупреждений нет.
- `cargo test --workspace --offline`, 3 прогона: каждый раз 143 passed, 0 failed, 1 ignored.
- Проверка на флейки: lib test binary cctg и `command_logs` по 40 прямых запусков, 0 падений.

**Новые тесты (`scratch/qa/tests/qa.rs`, 14 штук, все проходят):**
- `persistent_failure_gives_one_notice_per_command_no_loop`, `failure_then_recovery_next_command_works`, `stopped_scheduler_does_not_hang_worker`: notice одноразовый, worker живой.
- `too_long_on_third_chunk_rest_is_exact_tail`: 400 too long на 3-м куске. Ops: `[Send, Send, Send, Doc]`, принятые куски + документ == тело, в caption нет имени проекта.
- `too_long_everywhere_switches_once_and_stops`: на каждую команду ровно `[Send, Doc]`.
- `huge_reply_is_one_document_equal_to_library`: 10×5000 символов дают один документ, байты равны `render_full`, thread сохранён.
- `fixtures_match_library_with_prefix_and_n`: все jsonl-фикстуры transcript × (`/full 5 abcdef01`, `/brief 1`, `/BRIEF@BOT 2 ABCDEF`). Куски по порядку совпадают с `split_for_telegram(render_*(last_prompts(parse)))`.
- `resolver_newest_prefix_ambiguous_subagents`: самый свежий по mtime, смена лидера после `set_modified`. Более свежий UUID-файл в `<uuid>/subagents/`, UUID-файл прямо в root и UPPERCASE UUID не выбираются. Уникальный/неоднозначный/несовпавший префикс.
- `ambiguous_reply_has_no_project_names`: в notice нет `secretname`, `C--`, `proj`. Short id и ai-title есть.
- `bad_paths_notices_no_path`: каталог с именем сессии, нет сессий, нет совпадения, `../../etc`, отсутствующий root, невалидный UTF-8 с обрезанной последней строкой. Каждый случай получает свой notice без пути, worker идёт дальше.
- `parse_edges`: `/brief 2026` даёт Usage, `/brief 3 2026` даёт prefix, перевод строки как разделитель, `/brief@`, переполнение n.
- `restart_does_not_replay_and_stranger_is_ignored`: "краш" сразу после обработки батча, на диске 103. После рестарта Telegram отдаёт все старые апдейты плюс новый, обрабатывается только новый. Команда от пользователя не из allowlist не проходит.
- `stale_offset_is_ignored_fresh_is_used`: 25 ч даёт None, 23 ч даёт значение. mtime в будущем загружается, пробелы/CRLF допустимы, пустой файл даёт None.
- `telegram_id_reset_is_handled_once`: сохранено 900000, Telegram сбросил id до 3/4. Оба обработаны по одному разу, на диске 5, все последующие вызовы с offset 5.

**Логи (`scratch/qa/tests/logs.rs`, отдельный бинарник, глобальный subscriber, `.without_time()`):** poll плюс worker. Scheduler отвечает ошибкой, сохранение offset ломается, есть unknown `/sessions <marker> secret`, обычный текст и команда чужого пользователя. События `transcript command reply failed`, `cannot save the getUpdates offset` и `unknown slash command` есть. В логах нет маркера пути, обоих user id, username, `Users`, `secret` и текста сообщений. PASS.

**Probe по реальному `~/.claude/projects` (release, read-only):**
- 76 top-level сессий, `locate(None)` за ~0.85 ms. Ни одна найденная сессия не лежит под `subagents`.
- 16 неоднозначных notices через реальный `prepare`: имя проекта не утекло ни разу, все укладываются в 4096.
- 462 рендера (brief/full × n=1/3/100 на ~40 сессиях от маленьких до самой большой). Расхождений с библиотекой 0, кусков больше лимита 0, документов 125, худшее время 164 ms.
- Утечка thinking: 2 совпадения префикса thinking в теле. Оба ложные: тот же текст есть в обычном text/tool блоке. Реальных утечек 0.
- ai-title есть у 47 из 76 сессий, но в первых 64 KiB он находится только у 15 (см. баг 1).

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| `/brief`, `/full` на фикстурах совпадают с библиотекой, порядок кусков сохранён | `fixtures_match_library_with_prefix_and_n`, `too_long_on_third_chunk…`, существующие `replies_match_the_library_on_fixtures`/`multi_chunk_reply_keeps_order`, probe 462 рендера | PASS |
| Крупный вывод идёт документом; 400 too long один раз переключает на документ, без цикла | `huge_reply…`, `too_long_everywhere…`, `too_long_on_third_chunk…`, `rejected_fallback_document_is_not_retried` | PASS |
| Сохранённое смещение исключает повтор после рестарта | `restart_does_not_replay…`, `stale_offset…`, `telegram_id_reset…`; код: `save_offset` (spawn_blocking) ожидается до `for_each(handle)` | PASS |
| Нечитаемый/отсутствующий путь даёт понятное сообщение, поллинг жив | `bad_paths_notices_no_path`, `stopped_scheduler…`, существующие `a_slow_command_does_not_hold_up_polling`, `failing_offset_saves_do_not_stop_polling` | PASS |
| Нет токена, реальных user id и приватных путей в логах/тестах/фикстурах | `logs.rs`, `command_logs`, grep diff (`Users[\/-]user`, `AppData`, шаблон токена, числа ≥7 цифр): пусто; трейлеров в коммитах нет | PASS |
| Резолвер по корню проектов: свежий по mtime, префикс, кандидаты при неоднозначности, узкий интерфейс | `resolver_…`, `ambiguous_reply…`, probe; `TranscriptLocator::locate(thread_id, prefix)` | PASS |
| `subagents/*.jsonl` никогда не top-level | `resolver_…` (UUID-файл новее под subagents), probe: 0 из 76 | PASS |
| Existing tests pass | 3 × 143 passed / 1 ignored, 40× флейк-прогон | PASS |

Проверка фиксов fixer-а по коду и тестам:
- #1 notice о неудаче: одна попытка, без повторов, следующая команда работает. PASS.
- #3 offset через `spawn_blocking`: `save_offset(...).await` стоит до `for_each`, порядок "сначала сохранить" сохранён (`offset_is_saved_before_the_batch_is_handled`, мой restart-тест). PASS.
- #4 unknown commands: в лог идёт только первое слово, остальной текст не логируется. PASS.
- #5 кандидаты без имён проектов: подтверждено синтетикой и на реальных данных. PASS.

Не выполнено: живой round-trip `/brief` в Telegram и замер RSS под long poll. Нужны реальный бот и `.env`, по решению оркестратора это smoke после merge.

## 4. Bugs found

1. **Minor. Заголовки кандидатов почти всегда пустые.** `candidate_title` (`commands.rs:194`) ищет `ai-title` только в первых 64 KiB файла, а Claude Code дописывает `ai-title` позже. На реальных данных заголовок есть у 47 сессий, в первых 64 KiB он виден у 15. Воспроизведение: `cargo run --release` в `scratch/qa`, строка `ai-title present`. Ожидалось, что у большинства кандидатов с заголовком он будет показан. Фактически у ~2/3 его нет. Функционально не ломает выбор (short id и возраст есть), и в спецификации этого нет: это добавка fixer-а. Варианты: читать хвост файла или отказаться от заголовка.
2. **Low. Нет notice, если fallback-документ отвергнут с описанием "too long".** `send_delivery_failure_notice` пропускает любую too-long ошибку, включая ошибку самого документа после переключения. Воспроизведение: `too_long_everywhere_switches_once_and_stops`, Telegram на всё отвечает 400 "message is too long". Ops `[Send, Doc]`, пользователь не получает ни транскрипт, ни notice. Ожидался notice. На практике документ с "too long" отвергается только при длинном caption, а caption короткий, так что сценарий почти теоретический.
3. **Low. Первое слово неизвестной команды попадает в debug-лог целиком.** Если пользователь напишет, например, `/C:\Users\name\x`, этот токен окажется в логе. Уровень debug, данные идут от allowlisted пользователя, секретов hub тут нет. Упомянуто для полноты.

Инварианты токена/user id: `Routed`/`Inbound` не несут `from.id`. `ApiError` без URL (TASK-008). `io::Error` и `LocateError` в notices и логах только как `ErrorKind`. Тексты `with_context` в `run` называют env-переменную, а не путь. Путей утечки токена или user id не найдено.

## 5. Verdict

**SHIP.** Все 8 критериев приёмки выполнены и проверены независимыми тестами. fmt, clippy и тесты чистые, флейков нет. Фиксы fixer-а сделаны так, как заявлено. Найденные баги minor/low и в критерии приёмки не входят. Баг 1 стоит завести мелкой задачей или закрыть в TASK-011, где кандидаты всё равно будут перерисовываться.

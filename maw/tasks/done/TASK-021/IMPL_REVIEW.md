# Implementation Review — TASK-021

## Verdict

**NEEDS_WORK** — основная маршрутизация реализована и тесты проходят, но поздний `Reply` завершённой сессии может попасть в тему уже следующей сессии, а неизвестная slash-команда может утечь в лог.

## Confirmed correct

- Allowlist применяется до создания `Inbound`; сообщения из другого чата и без отправителя отбрасываются, а все `forum_topic_*` классифицируются как service messages и не попадают агенту ([updates.rs](../../../../crates/cctg/src/hub/updates.rs):96, [updates.rs](../../../../crates/cctg/src/hub/updates.rs):415).
- Неявный forum `reply_to_message` на корень темы удаляется, а явный reply сохраняется только как id ([updates.rs](../../../../crates/cctg/src/hub/updates.rs):119, [updates.rs](../../../../crates/cctg/src/hub/updates.rs):368).
- Inbound разрешается через `thread_id -> slot -> current live top-level session -> current agent`; General и неизвестные темы не пересылаются. Meta состоит только из допустимых ключей `chat_id`, `message_id`, `thread_id`, `reply_to_message_id` ([slots.rs](../../../../crates/cctg/src/hub/slots.rs):468, [slots.rs](../../../../crates/cctg/src/hub/slots.rs):493).
- Путь ingress/slots не ждёт Telegram: агент получает inbound через `try_send`, а Telegram-операции передаются отдельной dispatch-задаче. Тест со stalled transport подтверждает продолжение inbound ([slots.rs](../../../../crates/cctg/src/hub/slots.rs):501, [slots.rs](../../../../crates/cctg/src/hub/slots.rs):1339).
- Reply разбивается через `split_for_telegram`, сохраняет порядок частей и при `prefer_file` отправляется одним документом; тест покрывает и путь больше четырёх частей ([slots.rs](../../../../crates/cctg/src/hub/slots.rs):561, [slots.rs](../../../../crates/cctg/src/hub/slots.rs):1295).
- Очередь reply/notice ограничена 256 элементами; переполнение предупреждает один раз за эпизод. Notice cooldown раздельный по `(slot, kind)`, равен 60 секундам по умолчанию и offline notice re-arm происходит после успешной передачи inbound агенту ([slots.rs](../../../../crates/cctg/src/hub/slots.rs):589, [slots.rs](../../../../crates/cctg/src/hub/slots.rs):608, [overflow_logs.rs](../../../../crates/cctg/tests/overflow_logs.rs):90).
- `/clear` с pid переносит агент в новую сессию того же слота; inbound после clear покрыт тестом ([slots.rs](../../../../crates/cctg/src/hub/slots.rs):352, [slots.rs](../../../../crates/cctg/src/hub/slots.rs):1252).
- Обычные inbound/reply тексты и user id не появляются в новых routing-логах; отдельные integration tests используют subscriber без времени. stdout channel-сервера также остаётся JSON-RPC-only по существующим тестам ([message_logs.rs](../../../../crates/cctg/tests/message_logs.rs):75).
- `docs/poc.md` корректно применяет `--mcp-config`, `--strict-mcp-config` и изолированный `CLAUDE_CONFIG_DIR`; это соответствует текущей официальной CLI/settings/MCP документации Claude Code ([poc.md](../../../../docs/poc.md):68).
- `Cargo.toml` и `Cargo.lock` относительно `main` не изменены; новых зависимостей нет.
- Проверено локально с одним target-каталогом вне repo: `cargo fmt --all -- --check`, `cargo clippy -j 1 --workspace --all-targets -- -D warnings` и `cargo test -j 1 --workspace` завершились успешно. В library suite: 233 passed, 1 ignored; все integration и doc tests также прошли.

## Issues

### Major — поздний Reply завершённой сессии маршрутизируется в переиспользованный topic

- **Место:** [slots.rs](../../../../crates/cctg/src/hub/slots.rs):538 (особенно строки 543–548); связанное сохранение старого slot после end — [registry.rs](../../../../crates/cctg/src/hub/registry.rs):677 и переназначение `current_session` — [registry.rs](../../../../crates/cctg/src/hub/registry.rs):518.
- **Описание:** `on_reply` проверяет только то, что старая запись `SessionEntry.agent` всё ещё равна `conn`, затем использует сохранённый `SessionEntry.slot`. Он не проверяет `is_live_top_level(session)` и не сверяет, что `slot.current_session == session`. При обычном `SessionEnd` поле `agent` не очищается, а старый `slot` остаётся в записи. Если новая сессия занимает тот же слот, поздний Reply старого агента публикуется в тему новой сессии. Scratch-repro подтвердил это: `late_reply_delivered_to_reused_topic=true` ([repro](scratch/repro_reply/src/main.rs), [evidence](scratch/code-review-evidence.md)).
- **Suggested fix:** до построения `Op` требовать одновременно: сессия живая top-level, её `agent == Some(conn)`, и она является `current_session` своего slot. Альтернативно централизовать такую проверку в helper симметрично `live_agent`. Добавить regression test: A start/register → A end без disconnect → B занимает тот же slot → поздний Reply A не создаёт `Send`; затем Reply B идёт в этот topic.

### Major — неизвестная slash-команда раскрывает пользовательский текст/секрет в лог

- **Место:** [commands.rs](../../../../crates/cctg/src/hub/commands.rs):425–430.
- **Описание:** для `Parsed::NotOurs` первый токен любого текста, начинающегося с `/`, пишется как structured field `command`. Сообщение `/my-secret-token` поэтому попадает в лог дословно. Это противоречит acceptance criterion «ни текст сообщений, ни user id, ни секреты не пишутся в логи». Существующий `command_logs` не ловит случай: marker помещён вторым аргументом, тогда как в лог попадает только первый токен ([command_logs.rs](../../../../crates/cctg/tests/command_logs.rs):87).
- **Suggested fix:** не логировать значение неизвестной команды вообще (оставить только фиксированное сообщение/счётчик), либо логировать лишь закрытый allowlist известных нечувствительных command names. Добавить отрицательный log-test, где уникальный marker находится непосредственно в первом slash-токене.

### Minor — `docs/poc.md` содержит устаревшее утверждение о `--settings`

- **Место:** [poc.md](../../../../docs/poc.md):60.
- **Описание:** документ утверждает, что в CLI reference есть фраза о том, что hooks не грузятся из `--settings`. Текущая официальная CLI reference описывает `--settings` как session override для settings JSON без такого исключения, а settings docs прямо относят hooks к settings. Сам рецепт корректен, но предупреждение создаёт ложную неопределённость.
- **Suggested fix:** удалить утверждение про CLI reference либо явно привязать его к старой версии и дать ссылку/дату; fallback в `CLAUDE_CONFIG_DIR/settings.json` можно оставить как troubleshooting step.

## Missing coverage

- Reply от завершённой сессии после переиспользования её slot новой сессией — именно этот отсутствующий сценарий воспроизводит major bug.
- Reply от старого channel connection после `/clear`, когда rebinding по pid невозможен/отсутствует; он также должен быть отброшен, если old session уже не current.
- Явный тест stale duplicate connection: после регистрации нового conn старый conn пытается отправить Reply. Код сейчас корректно отбрасывает его через `agent == Some(conn)`, но отдельного regression test нет. Это был исходный disconfirmation-case; он не подтвердился.
- Privacy-test с уникальным marker непосредственно в неизвестном slash-command token (`/marker`), а не только в аргументе после команды.
- Атомарность лимита для многокускового Reply: при backlog 255 двухчастный Reply должен быть отброшен целиком, без первой части.

## Nits

- `IMPL_SUMMARY.md` сообщает 713 вставок/19 удалений, тогда как запрошенный `git diff main -- Cargo.toml Cargo.lock crates/ docs/` показывает 1208 вставок/19 удалений. Это не дефект runtime-кода, но summary неточно описывает review surface относительно заданной базы `main`.

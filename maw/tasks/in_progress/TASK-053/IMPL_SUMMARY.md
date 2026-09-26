# TASK-053 — IMPL_SUMMARY (implementer, small-fix)

Code commit: `54d25b7 feat: show context compaction in the topic status and feed (TASK-053)` on `feature/compact-status`.

## Pre-flight и проверка по документации

Все сущности из спеки существуют и выглядят так, как спека предполагает: `hook.rs::build` отвергал `PreCompact` (`Skip("unsupported hook event")`, тест `misrouted_and_unknown_events_are_skipped`), `wire::HookEvent` + `KINDS` + `decode_hook` (неизвестный kind = `UnknownKind` → 400 в ingress), статус TASK-029 (`hub/status.rs`, `Slots::status_view`/`pump_status`, метрики в `SessionEntry::metrics`), группы хуков в `install.sh` и `docs/hook-settings.json`, сверка в `install_e2e`.

Официальная документация (https://code.claude.com/docs/en/hooks, raw markdown сохранён в `scratch/hooks_doc.md`, строки 3056-3110 и 1154):
- `PreCompact`: matcher `manual` (`/compact`) | `auto`. Вход: общие поля + `trigger` и `custom_instructions` («For `manual`, `custom_instructions` contains what the user passes into `/compact` and is `null` when they pass nothing. For `auto`, `custom_instructions` is `null`»). «Exit with code 2 to block compaction… You can also block by returning JSON with `"decision": "block"`.» Наш хук всегда exit 0 и пустой stdout, поэтому сжатие не блокирует.
- `SessionStart.source`: `"compact"` after compaction.
- Есть ещё документированный `PostCompact` (`trigger`, `compact_summary`). Спека его не просит; конец сжатия берётся из `SessionStart source=compact`, как в спеке (решение в log.jsonl).
- Таймаут command-хука по умолчанию 600 с.

## 1. Что сделано

| Файл | Строк | Что |
|---|---|---|
| `crates/cctg/src/wire.rs` | +20/-1 | `HookEvent::PreCompact { trigger: Option<String> }`, kind `pre_compact` (аддитивно, без bump `VERSION`); тесты round-trip, kind-list, событие без trigger. Пример «неизвестного kind» в тесте сменён на `post_compact`. |
| `crates/cctg/src/hook.rs` | +54/-1 | `cctg hook PreCompact`: берёт только `trigger` (только `manual`/`auto`, иначе `None`); `custom_instructions` вообще не десериализуется. Короткий бюджет POST (300 мс, TLS 600 мс), как у статусных событий. Не спулится (`spool::keeps` только start/end). Тест, что текст пользователя не попадает в POST. |
| `crates/cctg/src/hub/registry.rs` | +3/-1 | `PreCompact` в registry ничего не меняет. |
| `crates/cctg/src/hub/status.rs` | +89 | `Phase::Compacting { auto, minutes }` → «🗜 Сжимаю контекст (авто\|вручную)…», с минутами «… 2 мин»; `compacting_line`, `compacted_line` («🗜 Контекст сжат за 42 с: 81% → 12%» или без процентов). Тест. |
| `crates/cctg/src/hub/slots.rs` | +359/-1 | `compactions: HashMap<session, Compaction>`. `PreCompact` live current-сессии слота с темой → статус + одна тихая строка (`message_op`, `notify: false`); повтор при идущем сжатии игнорируется. `SessionStart source=compact` → статус возвращается; строка «сжат за N с» ждёт до `COMPACT_NUMBERS_WAIT` = 10 с первый `StatusLine`, чей context отличается от последнего до конца сжатия (тогда «X% → Y%»), иначе уходит без процентов; сессия без statusline получает строку сразу. `SessionEnd` (и любое окончание сессии) и `COMPACT_MAX` = 15 мин снимают статус без строки. `next_deadline` будит актор на каждую целую минуту (таймер в статусе, ~1 edit в минуту), на ожидание цифр и на 15-минутный лимит. Два unit-теста. Доки модуля дополнены. |
| `crates/cctg/src/hub/ingress.rs` | 1 | тест 400 на неизвестный kind теперь на `post_compact`. |
| `install.sh`, `docs/hook-settings.json`, `docs/poc.md` | +3, +11, +2 | группа `PreCompact` с `"timeout": 5`. |
| `crates/cctg/tests/status_e2e.rs` | +167 | e2e: настоящий `cctg hook PreCompact` (через `common::cctg`) → реальный `serve_hooks` → `Slots` → фейковый Bot API. |
| `crates/cctg/tests/hook_cli.rs` | +72 | `PreCompact` в `every_event_reaches_the_hub`; «старый hub» (отвечает 400) → хук exit 0, пустой stdout, ничего не спулит, текст пользователя не в stderr; сниппет настроек знает `PreCompact` и таймаут 5. |
| `crates/cctg/tests/install_e2e.rs` | +25 | `EVENTS` + `PreCompact`; набор событий install.sh = набор docs; группа PreCompact совпадает с docs (кроме пути команды), timeout 5. |

## 2. Отклонения и ограничения

- Таймер в статусе в целых минутах, не в секундах: иначе edit каждые 5 с на всё время сжатия. Первые 60 с статус без числа.
- Время начала отдельно не показывается (часовой пояс hub на удалённом сервере не совпадает с пользовательским); длительность есть в строке конца.
- Строки сжатия идут обычным `message_op`, не через stream lane: относительно строк транскрипт-потока порядок не гарантирован (поток отстаёт на доли секунды).
- Если сессия закончилась, пока строка ждала цифр, строки нет (сжатие закончилось, но сессия уже мертва).
- После рестарта hub идущее сжатие забывается (состояние только в памяти, как `activity`).
- Сжатие, заблокированное чужим хуком пользователя, держит статус до 15 мин (сигнала отмены нет; `PostCompact` не приходит тоже).

## 3. Тесты

Все сборки с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, перед прогоном `touch crates/cctg/src/lib.rs`.

- `cargo fmt --all -- --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok, без предупреждений.
- `cargo test -j 1 --workspace`: 881 passed, 0 failed, 3 ignored (сводка `scratch/full_test.txt`).
- Новые: `hub::slots::tests::a_compaction_shows_in_the_status_and_its_end_is_one_line`, `…a_compaction_without_a_status_line_is_told_at_once`, `hub::status::tests::a_compaction_reads_as_its_trigger_time_and_numbers`, `hook::build_tests::a_compaction_carries_its_trigger_and_never_the_users_text`, `status_e2e::a_compaction_from_the_real_hook_shows_in_the_status_and_the_topic`, `hook_cli::a_hub_without_compactions_leaves_the_hook_quiet`.

Критерии приёмки:
- PreCompact auto/manual виден в статусе и одной строкой, конец даёт строку с длительностью и процентами: `status_e2e` (реальный `cctg hook`) + unit-тесты slots.
- `custom_instructions` не попадают ни в hub, ни в логи: хук не читает поле; `build_tests` проверяет тело POST, `status_e2e` проверяет stderr хука (RUST_LOG=trace) и все ops фейкового Bot API, `hook_cli` проверяет stderr при отказе hub.
- Старый hub не ломается: он отвечает 400 (`UnknownKind`, тест в `ingress`), хук молчит и выходит 0 (`hook_cli`). install.sh пишет группу PreCompact (`install_e2e`).

## 4. Как проверить вручную

1. Собрать, поставить клиент с новой группой (или добавить в `settings.json` блок `"PreCompact": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook PreCompact", "timeout": 5 }] }]`), обновить hub.
2. В сессии через `claude-cctg` набрать `/compact что-то личное`. В теме: закреплённый статус «🗜 Сжимаю контекст (вручную)…» (через минуту «… 1 мин»), в ленте тихая строка «🗜 Сжимаю контекст (вручную)…». Текста «что-то личное» нет ни в теме, ни в логе hub.
3. По окончании: статус возвращается к «💤 Ждёт вас» с цифрами, в ленте «🗜 Контекст сжат за N с: X% → Y%» (без процентов, если statusline не прислал новое число за 10 с).
4. Авто-сжатие даёт то же с «(авто)». Закрыть claude во время сжатия: статус «🏁 Сессия завершена», строки об успехе нет.

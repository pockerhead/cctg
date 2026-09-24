# TASK-016 IMPL_SUMMARY

Verdict: IMPLEMENTED (reference patch applied as-is, no manual edits).

## 1. Что сделано

Pre-flight (шаг 0): `git status --short` пуст, ветка `feature/hub-turn-streaming`, `git diff --stat e7b16b6 HEAD -- crates Cargo.toml Cargo.lock docs` пуст, `git apply --check scratch/reviewer2/task016.patch` проходит.

Шаг 1: `git apply scratch/reviewer2/task016.patch`, затем `bash scratch/reviewer2/verify_hashes.sh`: 23 строки `OK`, код 0.

Изменённые файлы (`git diff --numstat HEAD`, добавлено/удалено):

| Файл | +/- |
|---|---|
| crates/cctg/src/agent.rs | +128 / -3 |
| crates/cctg/src/channel.rs | +4 / -1 |
| crates/cctg/src/hub/api.rs | +13 |
| crates/cctg/src/hub/ingress.rs | +1 |
| crates/cctg/src/hub/mod.rs | +1 |
| crates/cctg/src/hub/registry.rs | +78 |
| crates/cctg/src/hub/scheduler.rs | +331 / -11 |
| crates/cctg/src/hub/slots.rs | +1312 / -11 |
| crates/cctg/src/lib.rs | +1 |
| crates/cctg/src/wire.rs | +135 / -3 |
| crates/cctg/tests/{ingress,message,permission,slots}_logs.rs | +1 каждый |
| crates/transcript/src/lib.rs | +2 |
| crates/transcript/src/render.rs | +3 / -3 |
| crates/transcript/tests/purity.rs | +2 / -1 |

Новые файлы (строк): `crates/cctg/src/hub/stream.rs` 498, `crates/cctg/src/tail.rs` 548, `crates/cctg/tests/stream_logs.rs` 223, `crates/transcript/src/stream.rs` 171, `crates/transcript/tests/stream.rs` 97, `crates/transcript/tests/fixtures/stream.jsonl` 12.

## 2. Отклонения от плана

Нет. Руками ничего не правилось. Мутационный прогон (`R/mutations.py`) не запускался: по плану он для QA.

## 3. Тесты

Env: `CARGO_TARGET_DIR=%TEMP%/cctg-impl016-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз; каталог удалён после прогона.

- `cargo test -j 1 --workspace --no-fail-fast`: код 0. `cctg` lib 358 passed / 1 ignored (как в эталоне), `stream_logs` 1 passed, `transcript` `tests/stream.rs` 4 passed, остальные бинари зелёные. Лог: `scratch/implementer/workspace_test.txt`.
- `cargo fmt --all --check`: код 0 (`scratch/implementer/fmt.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: код 0 (`scratch/implementer/clippy.out.txt`).

## 4. Ручная проверка

Живой smoke по плану (раздел 3, после merge): hub и одна интерактивная сессия с каналом по `docs/poc.md`, 2-3 Bash-вызова (два параллельных), сообщение из темы во время хода. Ожидается: строки `• Bash: ... ✓` в порядке вызовов, на сообщении 👀, затем ✍, финальный ответ после строк вызовов. Заодно посмотреть в транскрипте, какой записью пришло channel-сообщение во время хода (`queued_command` или meta `user`).

## 5. Известные ограничения (после ревью, fixer)

- Одна запись jsonl, которая сама больше `MAX_CHUNK_TEXT` или `MAX_CHUNK_ITEMS`, теряет хвост событий (`tail.rs`, `retain`/`truncate`): потерянный `Result` оставляет вызов открытым до следующего flush, потерянный `TurnEnd` держит ответ до `hold_answer`. Реальные сессии таких строк не пишут.
- `pump_streams` на каждом событии актора клонирует `stream.calls` из реестра и `transcript_path` для каждой потоковой сессии, даже когда `Live` уже есть (≤64 коротких строк, дёшево, но лишнее).
- Строка вызова `Agent` получает `✓`, когда приходит результат асинхронного запуска, а не когда субагент закончил; настоящее состояние показывает блок TASK-015.
- `tail.rs` держит в памяти одну запись до `MAX_RECORD` = 64 MiB (плюс lossy-копия и разбор) на чтение; строки 4-64 MiB читаются целиком, хотя поток их не показывает.
- (fixer, раунд 2) После отказа Telegram (не 4xx) планировщик держит поток темы "сломанным" до строки с `restart`. Её несёт первое сообщение нового `Live`, то есть сообщение после rewind. Если в тот же топик пишет уже другая сессия (ротация слота), её строки тоже отбрасываются неотправленными, пока её собственный rewind не пришлёт `restart`. Порядок сохраняется, но выходит один лишний цикл `stream_retry`.
- (fixer, раунд 2) Конец хода, прочитанный в транскрипте раньше `Stop`, теперь переживает начало следующего хода ещё `hold_answer`, а не обнуляется. Если `Stop` потерялся (hub не принял POST хука), ответ следующего хода в этом окне уйдёт сразу, возможно раньше своих строк вызовов.

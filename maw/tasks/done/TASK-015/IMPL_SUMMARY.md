# TASK-015 IMPL SUMMARY

## 1. Что реализовано

Pre-flight: дерево чистое, `08ba8ad` предок HEAD (`ffc7f9b`), коммиты после `c94b6ae` не трогают `crates/`, `docs/`, `Cargo.*`. `git apply --check` прошёл.

Применён `scratch/reviewer2/task015.patch` как есть, ручных правок нет. `verify_hashes.sh`: шесть `OK`, exit 0 (`scratch/implementer/hashes.out.txt`).

Файлы (git numstat, +/-):
- `crates/cctg/src/hub/mod.rs` +1/-0
- `crates/cctg/src/hub/registry.rs` +678/-33
- `crates/cctg/src/hub/slots.rs` +1199/-6
- `crates/cctg/src/hub/subagents.rs` новый, 695 строк
- `crates/transcript/src/subagent.rs` +19/-6
- `crates/transcript/tests/subagent.rs` +29/-0

Содержание по файлам описано в плане, шаги 3-7.

## 2. Отклонения от плана

Нет. `hook.rs`, `wire.rs`, `channel.rs`, `agent.rs`, `docs/hook-settings.json` не менялись.

Принятые ограничения (из плана, раздел 4):
- At-most-once первого send: tombstone значит, что блока в Telegram может не быть. Остаточная дыра: `sending=true` пишется асинхронно (`save_loop`), падение hub между передачей job и записью файла может дать один повторный send после рестарта.
- Субагенты сессий на другом устройстве блоков не получают, пока агент не отдаёт файлы. Nested-блоки работают везде.
- Родительский транскрипт, отставший больше чем на 60 с от последнего хука субагента, скрывает блок (без призрака). Субагенты nested run блоков не получают.
- Handback-отчёт только в памяти: рестарт между handback и stop даёт fallback на brief/last message. Body-чтение, прерванное рестартом, оставляет `в работе…` до конца сессии, потом `итог не получен`.
- Reply на блок, вытесненный `MAX_SUBAGENTS`, приходит без `target_agent`. При 1024 одновременно работающих блоках новый субагент блока не получает.
- Каждый блок стоит один metered send (+ unmetered edit) из общих ~20 сообщений в минуту группы.
- Первый старт новой версии удаляет из `registry.json` записи `subagents` без блока (TASK-011).
- Субагенты второго уровня (субагент запускает субагента) блока не получают: их вызов `Agent` лежит в `subagents/agent-<parent>.jsonl`, а скан смотрит только транскрипт сессии. Кандидат молча отпадает по окну, призраков нет (добавлено фиксером после review).

## 3. Тесты

Скрипт: `scratch/implementer/run_checks.sh` (один `CARGO_TARGET_DIR=%TEMP%/cctg-t015-impl-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, по одному cargo; каталог удалён после прогона). Выводы лежат рядом в `*.out.txt`.

- `cargo fmt --all --check`: exit 0.
- `cargo test -j 1 -p transcript --test subagent`: 15 passed.
- `cargo test -j 1 -p cctg --lib hub::`: 227 passed, 1 ignored (был до задачи).
- `cargo test -j 1 --workspace --no-fail-fast`: exit 0, cctg lib 311 passed / 1 ignored, все остальные бинарники ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0.

Flaky `one_slot_lives_through_hook_agent_end_and_the_next_session` в этом прогоне не падал.

## 4. Ручная проверка

Telegram не вызывался. Живой прогон: hub с `docs/hook-settings.json` (с hooks `SubagentStart`/`SubagentStop`/`PostToolUse SubagentHandback`), в сессии вызвать три субагента через `Agent`: в теме слота три блока `↳ <type> <id>: <description>`, `в работе…` меняется на отчёт. Bash-вызовы и `--agent` сессия блоков не дают. `claude -p` из Bash сессии: один `⇣ nested <id>` в теме родителя, новой темы нет. Reply на блок субагента: в сессию приходит `<channel ... target_agent="<id>">`. Рестарт hub: блоки редактируются, повторных send нет.

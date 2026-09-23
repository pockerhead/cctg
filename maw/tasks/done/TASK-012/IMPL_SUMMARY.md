# TASK-012 implementation summary

## 1. Что реализовано

Реализован `cctg hook <event>`: ограниченное по времени чтение stdin, узкая десериализация, сборка `HookPost`, один аутентифицированный HTTP POST без повторов и гарантированный `exit 0` без stdout. Поддержаны `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop`, а также `PreToolUse`/`PostToolUse` для `SubagentHandback`.

Добавлены device-конфиг без наследования секрета дочерними процессами, канонизация cwd, определение собственного и родительского Claude PID по дереву процессов, фильтрация внутренних subagent events, ограничение отчётов до 128 KiB и registration snippet без секретов/машинных путей.

Изменённые product/test-файлы (текущее число строк):

- `Cargo.lock` — 1790 строк
- `crates/cctg/Cargo.toml` — 26 строк
- `crates/cctg/src/device.rs` — 249 строк
- `crates/cctg/src/hook.rs` — 879 строк
- `crates/cctg/src/lib.rs` — 9 строк
- `crates/cctg/src/main.rs` — 86 строк
- `crates/cctg/src/proctree.rs` — 478 строк
- `crates/cctg/tests/fixtures/hook/post_tool_use_handback.json` — 24 строки
- `crates/cctg/tests/fixtures/hook/pre_tool_use_handback.json` — 19 строк
- `crates/cctg/tests/fixtures/hook/session_end.json` — 8 строк
- `crates/cctg/tests/fixtures/hook/session_start.json` — 9 строк
- `crates/cctg/tests/fixtures/hook/stop.json` — 12 строк
- `crates/cctg/tests/fixtures/hook/subagent_start.json` — 9 строк
- `crates/cctg/tests/fixtures/hook/subagent_stop.json` — 23 строки
- `crates/cctg/tests/fixtures/hook/subagent_stop_internal.json` — 19 строк
- `crates/cctg/tests/fixtures/hook/user_prompt_submit.json` — 9 строк
- `crates/cctg/tests/hook_cli.rs` — 289 строк
- `docs/hook-settings.json` — 75 строк

Evidence в task scratch:

- `scratch/verify_applied_hashes.ps1` — 28 строк; эквивалент LF-нормализованной SHA-256 проверки, 18/18 `OK`
- `scratch/inspect_fixture_line_endings.ps1` — 6 строк; подтверждает CRLF fixture на Windows
- `log.jsonl` — добавлены одна `dead_end` и одна `decision` запись implementer-а

## 2. Что не реализовано и отклонения от плана

Product-функциональность реализована полностью. Реальные `~/.claude/settings.json` и device/hub config не изменялись, Telegram API не вызывался, `.env` не читался — это явно запрещено задачей.

Единственное отклонение от готового reference: в `broken_input_is_skipped_without_panicking` перед перебором обрезанных префиксов удаляется конечный ASCII whitespace. На Windows checkout fixture имеет CRLF, из-за чего исходный тест видел два полных валидных JSON-префикса (`}` и `}\r`) и ошибочно ожидал не более одного process-tree probe. Runtime-код не изменялся; тест сделан независимым от line endings.

Предоставленный Bash `verify_hashes.sh` не стартовал в sandbox (`CreateFileMapping`, Win32 error 5). Его проверка повторена PowerShell-скриптом из `scratch/`; до дополнительной CRLF test-only правки все 18 хэшей совпали.

Предсуществующее изменение `maw/tasks/in_progress/TASK-012/metrics.md` не трогалось.

## 3. Результаты тестов

- `cargo test --workspace --offline -j 2` — 277 passed, 0 failed, 1 ignored
- `cargo test -p cctg --lib hook::build_tests::broken_input_is_skipped_without_panicking --offline -j 2` — 1 passed
- `cargo clippy -p cctg --all-targets --offline -j 2 -- -D warnings` — успешно, warnings отсутствуют
- `cargo fmt --all -- --check` — успешно
- `git diff --check` — успешно
- `scratch/verify_applied_hashes.ps1` — 18/18 файлов `OK` до документированной test-only правки

CLI integration suite включает шесть тестов и проверяет доставку всех событий, таймаут недоступного/молчащего hub, зависший stdin, битый ввод, отсутствие утечек и registration snippet. `SessionEnd` проверяется с границей `< 1.2 s`, при настроенных 300 ms stdin + 500 ms POST.

## 4. Как проверить вручную

Безопасная проверка без регистрации реальных hooks:

1. Выполнить `cargo test -p cctg --test hook_cli --offline -j 2`; ожидается `6 passed`.
2. Выполнить полный workspace command из раздела 3 для unit/regression coverage.
3. Проверить `docs/hook-settings.json`: шесть lifecycle-событий и `PostToolUse` matcher `SubagentHandback`; команды имеют вид `cctg hook <Event>`, секретов и абсолютных путей нет.
4. Не сливать snippet в реальный user settings в рамках этой задачи; живая Claude Code установка относится к следующей задаче.

# FIX SUMMARY — TASK-012

## Fixed

1. **Major: `claude.exe` больше не считается родителем только по имени.**
   - Windows по-прежнему делает один ToolHelp snapshot, после чего только для процессов в цепочке предков запрашивает `QueryFullProcessImageNameW` и `GetProcessTimes` через `PROCESS_QUERY_LIMITED_INFORMATION`.
   - Claude Desktop отбрасывается по точному case-insensitive правилу пути: `\WindowsApps\Claude_` или `\AnthropicClaude\`. Desktop-bundled CLI под `\Claude\claude-code\<version>\claude.exe` остаётся допустимым.
   - Каждый parent link принимается только если родитель создан строго раньше ребёнка. Отсутствующие query-данные, access denied, исчезнувший процесс или переиспользованный pid обрывают обход на первом непроверенном звене.
   - Непроверенный `CLAUDE_PID`, отсутствующий в подтверждённой цепочке, больше не используется как parent fallback.
   - Process table инъецируется синтетическими данными в unit-тестах. Покрыты Desktop Store, classic Desktop, bundled CLI, корректная цепочка, reused pid и недоступный parent.
   - Win32 handles обёрнуты в RAII и закрываются ровно один раз; каждый `unsafe` блок локален и снабжён `SAFETY`-обоснованием.
   - Включён только требуемый feature `windows-sys`: `Win32_System_Threading`.

2. **Minor: пустой process env больше не затеняет `device.env`.**
   - Значения process env нормализуются и проверяются на пустоту до fallback к файлу.
   - Тест одновременно проверяет secret, hook address и host override.

3. **Minor: фильтр `SubagentStart` усилен там, где payload это позволяет.**
   - Empty и whitespace-only `agent_type` отбрасываются одинаково для Start/Stop.
   - Тест фиксирует, что typed Start без готовых файлов проходит: перенос stop-фильтра по существованию файлов сюда сломал бы корректные асинхронные старты.
   - В коде документирован остаточный риск typed `--agent`: реальная fixture `SubagentStart` не содержит `agent_transcript_path`, поэтому надёжно отличить такой start от настоящего subagent текущими полями невозможно.

4. **Nit: hook stderr теперь без ANSI и timestamp.**
   - Только hook-режим получает `.with_ansi(false).without_time()`; формат hub/agent не изменён.
   - CLI-тест проверяет отсутствие escape sequence и timestamp prefix.

## Skipped

1. **Буквальный перенос файлового фильтра `SubagentStop` на `SubagentStart`.** Review сам отмечает, что к моменту Start файлы могут ещё не существовать, а проверенная fixture Start вообще не содержит `agent_transcript_path`. Такой fix отбрасывал бы корректные subagent starts; вместо него применён безопасный доступный фильтр и задокументирован residual risk.

2. **Linux branch “не собрана”.** Это не подтверждённый дефект исходников. Независимый `cargo check -p cctg --target x86_64-unknown-linux-gnu --offline` остановился до компиляции: target-граф требует отсутствующий в cache `openssl-probe v0.2.1`; сам Linux target на Windows-хосте не установлен. Сетевые загрузки в этой задаче недоступны.

3. **`serde_json::to_vec(...).expect("hook posts always serialize")`.** Не менялось: `HookPost` содержит только сериализуемые строки/числа, а фиксированный panic hook всё равно сохраняет exit 0 и не раскрывает payload. Review также не предъявляет достижимого контрпримера.

4. **Линейный поиск цикла в process chain.** Не менялся: глубина жёстко ограничена 64, поэтому это не практический дефект и не относится к requested fixes.

5. **Дополнительные missing-coverage предложения review.** Отсутствующий `hook_event_name` уже принимается тестом `source_is_optional_and_read_only_from_session_start`; timeout покрыт unit/integration tests. Отдельный CI-прогон через Git Bash не добавлялся: реальная регистрация hooks запрещена scope задачи, а review не показал связанного дефекта runtime-кода.

## Test results

- `cargo test --workspace --offline`
  - **PASS** — 280 passed, 0 failed, 1 ignored.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`
  - **PASS** — finished successfully, no warnings.
- `cargo build --workspace --offline`
  - **PASS** — workspace dev build completed.
- `cargo build -p cctg --release --offline`
  - **PASS** — release build completed.
- `cargo fmt --all -- --check`
  - **PASS** — no formatting differences.
- `git diff --check`
  - **PASS** — no whitespace errors (only expected Windows LF→CRLF notices).
- `maw/tasks/in_progress/TASK-012/scratch/fixer_measure_lineage.ps1`
  - **PASS** — 20 release hook launches: min 14.53 ms, median 14.91 ms, max 232.63 ms. The measurement includes process startup, stdin parsing, ToolHelp snapshot and the new image/time queries; stdout stayed empty and exit code stayed 0. The integration test `a_silent_hub_keeps_session_end_well_inside_its_budget` also passed.
- `cargo check -p cctg --target x86_64-unknown-linux-gnu --offline`
  - **NOT RUN TO COMPILATION** — missing cached `openssl-probe v0.2.1`; documented above under Skipped.

No hook was registered, no real Telegram call or real device configuration was used, and no commit was created. The pre-existing change in `maw/tasks/in_progress/TASK-012/metrics.md` was preserved untouched.

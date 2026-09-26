# TASK-053 — FIX_SUMMARY (fixer)

Все правки в `crates/cctg/src/hub/slots.rs`. Коммит `fix: end a cancelled compaction's status and tighten its done line (TASK-053)` на `feature/compact-status`.

## Preflight: самое опасное утверждение ревью

Finding 1 предлагает как «простейший безопасный вариант» просто удалять запись сжатия на `UserPromptSubmit`/`ToolStart`/`Stop`. Проверил по коду: хуки инструментов асинхронные (`status.rs::Activity::tool_start`: «the tool hooks run in the background»), это отдельные POST из отдельных процессов. Поэтому `ToolStart`/`Stop` могут прийти в hub раньше `SessionStart(compact)` даже при успешном авто-сжатии посреди хода. Удаление записи в этом случае теряет строку «сжат за N с». Поэтому я сделал вариант с окном (так решил и оркестратор), а не удаление.

## 1. Fixed

1. **Отменённое или упавшее сжатие висит в статусе до 15 мин** (major). Подтверждено: запись завершали только `SessionStart(compact)`, конец сессии или `COMPACT_MAX`, а `status_view` ставил `Compacting` выше activity. Что сделано:
   - у `Compaction` новое поле `settled: Option<Instant>`;
   - `compact_settled`: `UserPromptSubmit`, хук `ToolStart`, `Stop` или записанный Esc (ответ агента на ⏹, `on_key_written`) этой сессии при идущем сжатии ставят `settled`. `status_view` такую запись больше не показывает, статус сразу возвращается к обычному TASK-029;
   - `compact_ended` принимает `SessionStart(compact)` и для settled-записи, если прошло меньше `COMPACT_GRACE` = 30 с. Строка «сжат за N с» (с процентами) уходит как обычно;
   - `check_compactions` забывает settled-запись без строки после `COMPACT_GRACE`, `compaction_deadlines` будит актор к этому сроку;
   - новый `PreCompact` при settled-записи начинает новое сжатие;
   - строки транскрипт-потока сжатие не завершают: они отстают и могут описывать вызовы до `PreCompact` (решение в log.jsonl).

   Тест `activity_after_a_compaction_ends_its_status_and_a_late_end_still_counts` проверяет оба порядка. Активность, потом поздний `SessionStart(compact)` в окне: статус уходит с 🗜 сразу, строка «80% → 20%» есть. `SessionStart(compact)`, потом активность: строка «20% → 9%». Ещё там проверено, что отмена через prompt и через `ToolStart` не даёт строки, в том числе после окна.
2. **Устаревшее значение statusline выше `before` считалось «после»** (minor). Подтверждено: было `before != Some(context)`. Теперь `compact_numbers` принимает только `context < before`. Тест `only_a_smaller_context_is_the_one_after_a_compaction`: поздние 83% при before 80% не закрывают ожидание, 15% закрывают.
3. **Второй `PreCompact`, пока первый ждёт цифр, терял строку первого** (minor). Подтверждено: `insert` молча перезаписывал ended-запись. Теперь `compact_started` для записи с `done.is_some()` сначала вызывает `compact_told(session, None)`. Тест `a_new_compaction_tells_the_one_waiting_for_its_numbers_first`: порядок строк «вручную…», «сжат за 0 с», «авто…».
4. **Ожидание 10 с при metrics без context** (minor). Подтверждено: было `has_numbers = metrics.is_some()`. Теперь после `before = last.or(before)` строка уходит сразу, если `before` = `None`. Тест `a_compaction_without_a_context_percentage_is_told_at_once`: statusline только с моделью.

Модульная документация slots.rs дополнена абзацем про отмену и окно.

## 2. Skipped

- Nit «`"async": true` вместо sync-хука с `timeout: 5`»: это дизайн, и в спеке про него ничего нет. Не трогал.
- Missing coverage «⏹ во время авто-сжатия»: Esc как триггер добавлен (`on_key_written` → `compact_settled`). Отдельного unit-теста на него нет: нужна вся обвязка `key_asks` и агента с `console_keys`. Путь покрыт тем же `compact_settled`, что и в тесте с prompt/tool.
- `status_e2e` через настоящий `cctg hook SessionStart` с `source: compact`: этот путь хука не новый, ревьюер сам считает это приемлемым.

## 3. Test results

Все сборки: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, перед полным прогоном `touch crates/cctg/src/lib.rs crates/cctg/src/main.rs`.

- `cargo fmt --all -- --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 -p cctg --lib compact`: 8 passed (все тесты сжатия, 4 новых).
- `cargo test -j 1 --workspace --no-fail-fast`: три полных прогона. В каждом падало 2-5 **разных** e2e-тестов, которые запускают настоящий бинарник и меряют время: `install_e2e`, `hook_cli`, `question_hook_e2e`, `statusline_cli`, `update_e2e`, `spool_e2e` (`took < 1500ms`), `status_e2e`. Параллельно на машине шли чужие cargo-сборки (`gta_sim -j 2`, `voidrun_simulation -j 4`). Первый прогон упал ещё на линковке: LNK1104, exe занят. Каждую упавшую цель перезапускал отдельно, все зелёные: `install_e2e` 10/10, `hook_cli` 9/9, `spool_e2e` 4/4, `status_e2e` 8/8, `question_hook_e2e` 6/6, `statusline_cli` 5/5, `update_e2e` 3/3, `run_e2e`/`supervise_e2e` ok. Последний полный прогон: 876 passed, 2 failed (`hook_cli::every_event_reaches_the_hub`, `spool_e2e::a_silent_hub_costs_a_hook_one_budget_however_much_is_kept`, оба потом зелёные отдельно), 3 ignored. В lib-тестах, где живут новые тесты, падений не было ни в одном прогоне. Сводка: `scratch/fix_test_summary2.txt`.

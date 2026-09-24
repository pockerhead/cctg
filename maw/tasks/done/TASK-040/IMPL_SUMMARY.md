# TASK-040 IMPL_SUMMARY

**Verdict: IMPLEMENTED** (эталонный patch применён, hashes 34/34 OK, main с TASK-042 влит)

## 1. Что сделано

Коммиты на `feature/client-self-update`:
- `a0a6691` feat: эталон `scratch/reviewer2/task040.patch` применён `git apply --ignore-whitespace` на `ccc0177`; `verify_hashes.sh`: 34 × OK, код 0. Код не переписывался.
- `7054e16` merge: `main` (TASK-042, 46eb6b4..72bba8a) влит. Текстовых конфликтов нет (TASK-042 менял ветку `Disconnected` и добавлял `heir`/`rebind`, `on_agent` не трогал). Правки стыковки внутри merge-коммита:
  - `crates/cctg/src/hub/slots.rs`: `heir()` пропускает соединения с `leaving` (TASK-040: уходящий рабочий "bound to nothing"). Без этого `rebind()` из TASK-042 возвращал сессию уходящему рабочему, когда новый рабочий отваливался. Тест `a_leaving_agent_is_never_the_heir_of_a_closed_link` сначала написан и упал (`left: Some((A, 1))`), после правки зелёный.
  - `crates/cctg/tests/update_e2e.rs`: запуск копии cctg через `common::isolate` (ручные `env_remove`/`HOME` убраны, helper делает то же и больше).
  - `crates/cctg/tests/run_e2e.rs`: запуск через `common::cctg(&home)`, импорт `Command` убран.
  Hashes после merge не сверялись (план так и говорит), сверялись тесты.

Итог diff против `ccc0177` по коду: 43 файла, +3920/-918 (из них TASK-042: `tests/common/mod.rs`, `tests/isolation.rs` и правки тестов). Новые файлы задачи: `src/client.rs` 73, `src/deploy.rs` 478, `src/run.rs` 197, `src/shim.rs` 223, `src/update.rs` 583, `tests/update_e2e.rs` 297, `tests/run_e2e.rs` 128. Крупные правки: `hub/slots.rs` +672, `supervise.rs` -831/+ (переписан), `keys.rs` +286, `agent.rs` +272, `wire.rs` +147, `hub/status.rs` +101, `main.rs`, `docs/poc.md`, `CLAUDE.md`.

## 2. Отклонения от плана

Нет по содержанию. Добавлен только фильтр `leaving` в `heir()` из TASK-042 (стыковка двух поведений, записано в `log.jsonl` как decision). Мутации (`mutations.py`) не перепрогонялись: код TASK-040 совпадает с эталоном по hashes, эталонный прогон 15/15 KILLED в `scratch/reviewer2/mutations.out.txt`.

## 3. Тесты

Все с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`.
- `cargo fmt --all --check`: чисто.
- `cargo clippy --workspace --all-targets -j 1 -- -D warnings`: чисто.
- `cargo test -p cctg --lib -- update:: run:: keys:: wire:: hub::slots::tests::{an_outdated,a_refused,without_its_own,a_leaving,a_short_lived,a_link_of_another} hub::status`: 48 passed.
- `cargo test -p cctg --test update_e2e --test run_e2e --test supervise_e2e --test isolation`: `a_new_binary_is_taken_without_losing_a_line ok`, `run_e2e: ok`, `supervise_e2e: ok`, isolation 2 passed (guard видит новые тесты).
- `cargo test --workspace -j 1 --no-fail-fast` (`scratch/implementer/workspace_test.txt`): 653 passed, 1 failed, 3 ignored, плюс `run_e2e: ok`, `supervise_e2e: ok`. Единственное падение `transcript --test purity every_source_file_is_scanned`: `read_dir(CARGO_MANIFEST_DIR/src)` NotFound, бинарник теста в общем target был собран из другого worktree (путь вшит на компиляции, cargo не пересобрал). После `touch crates/transcript/tests/purity.rs` и пересборки: 3 passed (`scratch/implementer/purity_rerun.txt`). К задаче не относится.

## 4. Ручная проверка (только с пользователем, агентам окна запрещены)

Порядок включения из плана, раздел 4 (release, установка старым `deploy`, перезапуск supervisor, замена обёрток `claude-cctg` на `cctg run --`). Затем: сессия через `claude-cctg`, в теме одно громкое предупреждение и строка с кнопкой в закрепе после `deploy` новой сборки; нажатие «Обновить» вне хода подменяет рабочий агент без перезапуска claude; изменение `--settings` файла + «Обновить» перезапускает claude в том же окне с `--resume <id>`, диалог каналов отвечен сам. Проверено автоматически только conhost-путь `/exit` из проб; Windows Terminal/mintty не проверены.

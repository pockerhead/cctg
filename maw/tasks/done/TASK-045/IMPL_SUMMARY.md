# TASK-045 — IMPL_SUMMARY

Коммит `d4cd632` на ветке `feature/join-codes` (worktree `C:/Users/user/dev/cctg-045`, от `main` 4c642fb с TASK-053).

## 1. Что сделано

Pre-flight: `git apply --check --ignore-whitespace` эталона на текущем дереве прошёл, поэтому оба патча применены напрямую, без `--3way`:
`scratch/planner/task045.patch`, затем `scratch/plan-reviewer-2/amendments.patch` (A1 `drop(permit)` убран в `Route::Join` + тест, A2 строка doctor, A3 docs).

Файлы (`git diff --stat` против 4c642fb): 18 файлов, +2792 / −116.

| Файл | Строк |
|---|---|
| crates/cctg/src/hub/devices.rs (новый) | +826 |
| crates/cctg/src/hub/ingress.rs | +433 / −69 |
| crates/cctg/src/hub/roster.rs (новый) | +488 |
| crates/cctg/src/join.rs (новый) | +344 |
| crates/cctg/tests/join_e2e.rs (новый) | +201 |
| crates/cctg/tests/install_e2e.rs | ~104 |
| crates/cctg/src/hub/mod.rs | ~103 |
| crates/cctg/src/wire.rs | +82 |
| install.sh | ~77 |
| crates/cctg/src/hub/config.rs | ~57 |
| crates/cctg/src/main.rs | ~49 |
| docs/remote-hub.md | ~30 |
| crates/cctg/tests/hub_reads_no_files.rs | ~17 |
| crates/cctg/src/doctor.rs | ~11 |
| README.md, deploy/hub.env.example, CLAUDE.md, lib.rs | мелочи |

`verify_final.sh`: 14 OK, 4 MISMATCH (`hub/ingress.rs`, `wire.rs`, `tests/install_e2e.rs`, `install.sh`). Это ровно те файлы, которые трогала TASK-053. `scratch/implementer/drift_check.sh` собирает эталонный результат на 7748a43 и сравнивает: у всех четырёх наш результат отличается от эталона ровно на дельту TASK-053 (7748a43 → 4c642fb), не считая номеров строк (`SAME-AS-TASK-053-DELTA` ×4). Своей правки поверх эталона нет.

Своё ревью против плана (прочитаны diff ingress/mod/config/main/doctor/install.sh, devices.rs `check/join/enroll/revoke/take_code/sweep`, roster.rs, join.rs): A1 на месте (permit живёт до конца `join_request`, включая паузу 250 мс), A2 одной строкой, `check` сравнивает общий секрет через `Secret::matches`, а хеш устройства через `ct_eq`. Подписка на `watch` стоит до проверки секрета и в агенте, и в хуке. `JoinError::Refused` даёт одинаковый 403 после паузы. Новых зависимостей нет (`aws-lc-rs` и `subtle` уже были). Расхождений с планом не нашёл.

## 2. Что не сделано / отклонения

Отклонений от плана нет. Ветка называется `feature/join-codes` (так в задании оркестратора), в TASK_FINAL указана `feature/device-enrollment`.

## 3. Тесты

Общий target, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, touch `lib.rs`, `main.rs` и трёх файлов тестов.

- `cargo fmt --all -- --check`: OK.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: exit 0 (`scratch/implementer/clippy.out.txt`).
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/implementer/test.out.txt`): 906 passed, 4 failed, 3 ignored, 43 бинарника. Упали `statusline_cli::without_a_command_or_inside_one_cctg_prints_its_own_line` и 3 теста `update_e2e`. Причина: общий target. Во время прогона другой worktree (TASK-056, коммит 35ee100 `feature/builtin-statusline`) пересобрал бинарник `cctg`: build id в assert был `35ee100a…-dirty`, а ожидался `4c642fb…-dirty`, statusline выдал вывод его версии. После touch перепрогнал эти два бинарника отдельно: `cargo test -p cctg --test statusline_cli --test update_e2e` дал 5/5 и 3/3 ok (`scratch/implementer/rerun.out.txt`).
- Все обязательные имена из PLAN_FINAL §2 п.5 в выводе есть и `ok`: `a_device_joins_with_a_code`, `a_device_joins_with_a_code_and_is_out_after_a_revoke`, `a_refused_join_keeps_its_request_place_through_the_pause`, `a_join_code_buys_one_device_secret_and_nothing_else`, `a_revoked_device_loses_its_link_and_its_hooks_at_once`, `a_waiting_hook_of_a_revoked_device_is_dropped_unanswered`, `with_the_shared_secret_off_only_devices_get_in`, `the_list_asks_before_it_revokes_and_the_revoke_cuts_the_device`, `the_guard_sees_a_file_read`.

## 4. Ручная проверка

Живую проверку из pipeline не делал (без Telegram и сервера). Шаги в PLAN_FINAL §3:
1. На hub: `docker compose exec hub cctg hub code` печатает `XXXX-XXXX-XXXX-XXXX`.
2. На устройстве: `~/.cctg/bin/cctg join КОД`, потом `cctg doctor` показывает «secret: set, this device's own (device …)». Повтор того же кода: выход 1 и «refused the join code».
3. `/devices` в General: устройство в списке, «Отозвать» → «Да, отозвать». Потом `cctg doctor` пишет «rejected the secret», сессия теряет агента.
4. Миграция по `docs/remote-hub.md` «Устройства и коды»: `cctg join` на каждой машине, перезапуск старых сессий claude, `CCTG_SHARED_SECRET=off`, перезапуск hub.

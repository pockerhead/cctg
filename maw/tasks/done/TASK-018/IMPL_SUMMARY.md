# TASK-018 IMPL_SUMMARY

## 1. Что сделано

Pre-flight: продуктовый код не менялся с `114e786` (`git diff --stat 114e786 HEAD -- crates docs Cargo.toml Cargo.lock` пуст), `git apply --check` прошёл. Применён `scratch/reviewer2/task018.patch`, `verify_hashes.sh` напечатал `OK` для всех 10 файлов, exit 0. Правок сверх патча нет.

Изменённые файлы (git numstat, +/-):
- `crates/cctg/Cargo.toml` +6
- `crates/cctg/src/agent.rs` +108
- `crates/cctg/src/device.rs` +32 -3
- `crates/cctg/src/hook.rs` +165 -3
- `crates/cctg/src/hub/testdir.rs` +79 -2
- `crates/cctg/src/lib.rs` +1

Новые файлы (строк):
- `crates/cctg/src/spool.rs` 555
- `crates/cctg/tests/soak.rs` 1858
- `crates/cctg/tests/spool_e2e.rs` 446
- `docs/soak.md` 69

## 2. Что не сделано

Живой soak (`CCTG_SOAK_LIVE=1`) не запускался по указанию оркестратора: его делает оркестратор или пользователь после влития при остановленном hub (команда в плане, раздел 3). Мутационный прогон не повторялся (эталонные результаты в `scratch/reviewer2/mutations.out.txt`). Отклонений от плана нет.

## 3. Тесты

Один `CARGO_TARGET_DIR=$TEMP/cctg-018-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, `--offline`, команды по очереди; каталог удалён в конце. Логи в `scratch/implementer/`:

- `cargo fmt --all --check`: exit 0 (`fmt.txt`)
- `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings`: exit 0 (`clippy.txt`)
- `cargo test --workspace --no-fail-fast -j 1 --offline`: exit 0 (`workspace_test.txt`). cctg lib 407 passed, 1 ignored (был и раньше); spool_e2e 4; hook_cli 7; stream_e2e 11; остальные цели зелёные; `soak: skipped`.
- `cargo test -j 1 --offline -p cctg --test soak -- --ignored` (fake, один прогон): exit 0, `soak: ok` (`soak_fake.txt`). 15.2 с, 104 вызова Telegram, 2 запуска hub; 3 createForumTopic, 1 разделитель; 14 service messages показано и удалено, 0 осталось; 2 x 429 (retry_after 1 с), после каждого пауза всей очереди и один повтор; permission A 15 мс (другая тема), A #2 61 мс (своя тема за стримом), 28 строк всплеска A #2, записанных до запроса, ушли после prompt; пик бакета 92%, минимальный зазор 30 мс; реестр: A = a5a5a5a5, A #2 = a2a2a2a2, B = b1b1b1b1, nested ee0e0e0e -> a1a1a1a1.
- После прогона: `%TEMP%/cctg-test-*` 0 каталогов, `%TEMP%/cctg-soak-*` нет.

Задержка permission A #2 (61 мс) ниже эталонной (189-201 мс): машина была менее загружена, утверждения теста от абсолютного значения не зависят.

## 4. Ручная проверка

1. `cargo test -p cctg --test soak -- --ignored` печатает отчёт и `soak: ok`.
2. Спул: остановить hub, выполнить `cctg hook SessionStart` с JSON сессии на stdin, увидеть файл в `~/.cctg/spool/<session>/`; запустить hub, выполнить `cctg hook UserPromptSubmit` той же сессии: hub получает сначала start, потом prompt, каталог сессии исчезает.
3. Живой прогон по `docs/soak.md` (hub остановлен, `CCTG_SOAK_LIVE=1`), в группе после него не остаётся тем `[soakbox]`.

## 5. Известные ограничения (добавлено на стадии fixer)

- Окно около 500 мс при возврате hub: если hub поднялся, пока `SessionStart`-хук ещё ждёт свой timeout, агент может зарегистрироваться и сделать replay до того, как хук положил событие в спул. Тогда сессия остаётся невидимой до своего следующего хука (обычно `UserPromptSubmit` или `Stop`): агент повторяет replay только после новой регистрации.
- Порядок и возраст файлов спула считаются по `SystemTime` (имя `<nanos>-<event_id>`). Если часы уйдут назад между двумя сохранениями одной сессии, replay отправит их не в том порядке, а возраст для 24 ч и `TMP_GRACE` посчитается неверно.

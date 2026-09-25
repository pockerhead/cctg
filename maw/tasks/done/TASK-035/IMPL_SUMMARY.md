# TASK-035 IMPL_SUMMARY (implementer)

Verdict: IMPLEMENTED. Code commit `c3128ab` on `feature/remote-hub` (not pushed).

## 1. Что сделано

Шаг 0 плана: `git apply --ignore-whitespace scratch/reviewer2/task035.patch` на чистое дерево (HEAD `50a9408`, код равен `e5bcc7f`), применился без правок руками. `verify_hashes.sh`: 43 x OK. Повторная проверка после мутаций: снова 43 x OK. Шаги 1-10 плана это содержимое патча, руками не повторялись.

Одно отклонение от эталона (дефект, доказан прогонами, см. п. 2): `crates/cctg/tests/message_logs.rs`.

Файлы коммита (`git show --numstat`), +добавлено -удалено:

| файл | строки |
|---|---|
| crates/cctg/src/tls.rs (новый) | +602 |
| crates/cctg/src/hub/ingress.rs | +496 -75 |
| crates/cctg/tests/tls_e2e.rs (новый) | +452 |
| crates/cctg/tests/proxy_e2e.rs (новый) | +321 |
| crates/cctg/src/hook.rs | +232 -60 |
| .github/smoke/smoke.py, compose.yml, fake_bot.py (новые) | +228, +38, +102 |
| .github/workflows/ci.yml, release.yml (новые) | +78, +135 |
| docs/remote-hub.md (новый) | +148 |
| crates/cctg/build.rs (новый) | +107 |
| crates/cctg/src/client.rs | +95 -13 |
| crates/cctg/src/hub/mod.rs | +86 -2 |
| crates/cctg/src/agent.rs | +75 -43 |
| crates/cctg/src/device.rs | +70 |
| deploy/compose.yml, compose.host.yml, hub.env.example (новые) | +66, +11, +8 |
| Dockerfile (новый), .dockerignore (новый) | +52, +8 |
| crates/cctg/src/hub/config.rs | +51 |
| crates/cctg/src/hub/slots.rs | +44 -3 |
| crates/cctg/tests/ingress_logs.rs | +32 -6 |
| crates/cctg/tests/slots_logs.rs | +17 -1 |
| crates/cctg/src/main.rs | +16 -1 |
| crates/cctg/src/wire.rs | +16 -4 |
| crates/cctg/tests/files_e2e.rs | +15 -1 |
| crates/cctg/tests/common/mod.rs | +13 |
| crates/cctg/tests/message_logs.rs | +13 -2 (правка implementer-а) |
| crates/cctg/src/statusline.rs | +10 -4 |
| crates/cctg/src/proctree.rs | +8 -1 |
| crates/cctg/tests/status_e2e.rs | +8 -3 |
| crates/cctg/src/spool.rs | +6 -4 |
| crates/cctg/Cargo.toml | +6 |
| crates/cctg/tests/update_e2e.rs | +5 -3 |
| crates/cctg/tests/supervise_e2e.rs | +4 -4 |
| crates/cctg/src/hub/status.rs | +3 -2 |
| crates/cctg/src/shim.rs | +3 |
| crates/cctg/src/hub/testdir.rs | +2 -1 |
| .gitignore | +2 |
| crates/cctg/src/lib.rs, CLAUDE.md | +1, +1 |
| Cargo.lock | +262 -5 |

Всего 44 файла, +3948 -238.

## 2. Отклонения от плана

**`tests/message_logs.rs` (найдено и исправлено).** С патчем тест `message_logs_carry_no_text_and_no_user_id` падал примерно в каждом втором прогоне (4 из 8 подряд, `the kept message is saved`, строка 173). На чистом HEAD в отдельном worktree с тем же общим target прошёл 6 из 6. Тест читал `registry.json` один раз сразу после того, как увидел `QUEUED_NOTICE` в фейковом транспорте. Но registry пишет отдельная задача `save_loop` (`hub/slots.rs:5421`) после того, как актор уже пошёл дальше, так что уведомление ничего не обещает про диск. Патч поменял тайминги, и гонка стала видна. Правка: опрашивать файл каждые 10 мс, пока в нём не появится сохранённое сообщение (лимит 5 с); проверка "user id нет в registry" осталась как была. После правки 5 из 5 плюс полный прогон зелёные. Код продукта не менялся. Лог: запись `decision`. PCTX: предложение 8.

Больше при чтении патча реальных дефектов не нашлось. Прочитал: `tls.rs`, `ingress.rs`, `device.rs`, `hook.rs`, `agent.rs`, `client.rs`, `build.rs`, `hub/mod.rs`, `hub/config.rs`, `spool.rs`, `statusline.rs`, `status.rs`, `Dockerfile`, `ci.yml`.

## 3. Тесты

Всё с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, по одной команде cargo, `-j 1`.

- `cargo fmt --all -- --check`: чисто, в том числе после правки message_logs.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: чисто, в том числе после правки.
- `cargo test -j 1 --workspace --locked --no-fail-fast`, финальный прогон (`scratch/implementer/workspace_test.final.txt`): 40 тестовых бинарников, 758 passed, 1 failed, 3 ignored. Упал `update_e2e::a_new_binary_is_taken_without_losing_a_line`: во время прогона другая стадия пересобрала `target/debug/cctg.exe` (`--version` после прогона показал чужой коммит `a53e49b...-dirty`). После `touch crates/cctg/src/main.rs` повтор зелёный (`workspace_test.final.rerun.txt`), бинарник показывает `9f855d0...-dirty`. Итог: 759 passed, 0 failed, 3 ignored, это и ожидал план.
- Первый полный прогон (`workspace_test.txt`, до правки): 756 passed, 3 failed. Две причины: чужая пересборка (`update_e2e`, и `transcript purity::every_source_file_is_scanned` с чужим `CARGO_MANIFEST_DIR`; оба зелёные после touch, `workspace_test.rerun.txt`) и гонка в `message_logs` из п. 2.
- Мутации: `python scratch/reviewer2/mutations/run_mutations.py C:/Users/user/dev/cctg` дал 19 из 19 KILLED, survived: none (`scratch/implementer/mutations.log`). Скрипт вернул все файлы: хеши совпали, `git status` по `crates` чистый после коммита.
- `target/debug/cctg.exe --version` показал `cctg 0.1.0 (9f855d0d0f515c89201388df722c51f78a1f4eb9-dirty)`. Сборка была до коммита, поэтому `-dirty`.

Не прогонялось здесь: Linux, macOS, Docker-образ и контейнерный smoke (Docker и WSL выключены). Проверка будет в первом прогоне `ci` на запушенной ветке. Push делает orchestrator.

## 4. Как проверить руками

1. `cargo test -p cctg --test tls_e2e`: настоящие hub, hook и agent по TLS с самоподписанным сертификатом. С чужим pin и без pin до hub не доходят. Pin есть в логе, секрета и токена в логе нет.
2. `cargo test -p cctg --lib -- tls:: device:: hub::ingress::tests client::` проверяет pin, правило "plain только на loopback", защиту до секрета и правило "сборка = коммит".
3. `target/debug/cctg --version` показывает коммит. `CCTG_BUILD_ID=abc cargo build -p cctg` вшивает `abc`.
4. Локальная схема без изменений: без `CCTG_HUB_CERT_SHA256` и с адресами `127.0.0.1` все прежние e2e идут по plain TCP. Живой hub пользователя в `~/.cctg` не трогался.
5. Сервер: `docs/remote-hub.md` (сертификат, `hub.env`, `docker compose up -d`, pin из лога в `device.env`).
6. CI: после push смотреть `test (ubuntu-latest|windows-latest|macos-latest)` и `image`. Как разбирать красное, написано в PLAN_FINAL раздел 3.

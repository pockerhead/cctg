# TASK-035 FIX_SUMMARY (fixer)

Ревью: `IMPL_REVIEW.md` (SHIP, 8 minor). Код: коммит `72f2e50` на `feature/remote-hub`.

## Проверка перед правками

Самое опасное место, если делать как написано в ревью: п. 2 предлагает писать на debug всё до секрета, кроме `Auth`. Проверил `hub/ingress.rs`. Под `agent rejected` попадает и `Rejection::Version`, то есть настоящий агент на другой версии протокола. А `hook request rejected` на строке 644 покрывал и отказы после секрета (`accept_hook`: битое тело от собственного хука). Если сделать дословно, эти два полезных сигнала пропадут. Поэтому сделал по формулировке задачи: до секрета один warn в минуту с одного IP, остальное на debug. Отказы после секрета остались warn.

## Fixed

1. **Лимит до секрета по IP.** `hub/ingress.rs`: `PerPeer` считает места по `IpAddr` (IPv4-mapped приводится к IPv4). Агент: `MAX_PENDING_AGENTS_PER_PEER = 4`, место отдаётся после `register`. Хук: `MAX_PENDING_HOOKS_PER_PEER = 16` (глобально 64), место держится только пока читается запрос. Ожидающий `PermissionRequest` его не держит. Loopback не исключён, иначе лимит на одной машине не проверить тестом. Тесты: `unauthenticated_agents_of_one_address_beyond_its_cap_are_closed_at_once`, `pending_hook_requests_of_one_address_beyond_its_cap_are_closed_at_once`, `places_per_address_are_counted_and_given_back`. Глобальный тест на 16 теперь открывает соединения с 127.0.0.2..5 и стоит под `cfg(not(target_os = "macos"))`: на macOS есть только 127.0.0.1. Доку «Защита до секрета» обновил, про NAT написал там же.
2. **Шум до секрета.** `WarnGate` + макрос `pre_auth!`: первый отказ с IP за 60 с идёт в warn, остальные в debug. Помнит до 1024 адресов, сверх этого всё идёт в debug. Через него идут `agent rejected`, `agent handshake timed out`, `too many agent connections`, 401/400/404/405 и таймаут хука до секрета, `too many hook requests`. В лог по-прежнему попадают только адрес и фиксированный текст. Тесты: `an_address_warns_at_most_once_a_minute`; в `tests/ingress_logs.rs` проверяется ровно 1 WARN и 3 DEBUG `agent rejected`, 1 WARN за неверный секрет хука и 2 DEBUG за битые заголовки.
3. **smoke.py без зависаний.** У `run()` таймаут 300 с, при срабатывании выход `FAIL: timed out ... <команда>`. Stdout агента читает поток в `queue`, основной поток ждёт с дедлайном, выход `FAIL: the message did not reach the agent within 30 s` или `closed its stdout`. У хука и raw-регистрации теперь понятный `FAIL` при таймауте, `agent.wait(timeout=30)`. Проба `scratch/fixer/smoke_timeout_probe.py` (+`.out.txt`): молчащий процесс даёт FAIL через 2.0 с, `run(timeout=1)` даёт FAIL.
4. **Первый деплой без `:latest`.** `docs/remote-hub.md`, «Один раз в GitHub»: пуш в main публикует `main` и `sha-<коммит>`, `latest` появляется только с `v*`. Варианты: первый тег `v0.1.0` на зелёный коммит или `:main` в `compose.yml` до релиза. Комментарий об этом добавлен и в `deploy/compose.yml`.
5. **`chmod +x` на Linux.** Добавлено в «Устройство» (Linux и macOS после каждого скачивания) и в порядок обновления в «Версии клиента и hub».
6. **Plain только к loopback-пиру.** `tls.rs`: `HubAddr::connect` без TLS проверяет `peer_addr()` через `plain_peer_allowed` (`to_canonical().is_loopback()`). Иначе `PermissionDenied`, соединение закрывается до отправки байта. Тест `plain_tcp_goes_only_to_a_loopback_peer`. Реальный тест с не-loopback слушателем не делал: на Windows bind на LAN-адрес может показать диалог firewall. Дока обновлена.
7. **`short()` различает грязные сборки.** `client.rs`: `<8>-dirty.<первые 4 символа хеша>`. Берутся символы, а не байты, поэтому чужая строка не паникует. Без хеша остаётся `-dirty`. Тест в `local_changes_and_no_git_fall_back_to_the_file`.
8. **update_e2e доказывает новую сборку и на чистом коммите.** `write_newer`: при чистом `SOURCE` в копии переписываются все вхождения закоммиченного id. Длина та же, символы сдвинуты. На macOS после этого `codesign --force --sign -`. При грязном или пустом `SOURCE` остаётся хвост. Теперь есть `assert_ne!(newer_build, first_build)`, а новый worker обязан сообщить `newer_build`. Чистый путь проверен: полный прогон шёл на чистом `72f2e50` (`cctg --version` = HEAD), тест зелёный. Грязный путь зелёный до коммита.

## Skipped

- Nits из раздела 5 (`needs` для `release`, git 2.31, новые крейты, healthcheck до `getMe`) не входили в scope. Кода не трогал.
- Missing coverage: прогон `release.yml` на тестовом теге и macOS clippy требуют CI или тега. Здесь их не проверить: Docker, WSL и пуши запрещены.
- Риск по п. 6 остаётся: `localhost` без pin, который резолвится в не-loopback, теперь отказывает. Проверено только unit-тестом на функции.
- Риск по п. 8 для macOS: переподпись через `codesign` здесь не проверить, нет Mac. Её проверит первый CI.

## Test results

Все команды с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`:

- `cargo fmt --all -- --check`: чисто.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: `Finished`, предупреждений нет.
- `cargo test -j 1 -p cctg --locked --lib -- ingress:: tls:: client::`: 39 passed.
- `cargo test -j 1 -p cctg --locked --test ingress_logs --test update_e2e --test tls_e2e` (грязное дерево, хвостовой путь): 3/3 ok.
- `cargo test -j 1 --workspace --locked --no-fail-fast` на чистом `72f2e50`: exit 0, **764 passed, 0 failed, 3 ignored**. Вывод в `scratch/fixer/workspace_test.txt`.

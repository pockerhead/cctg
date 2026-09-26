# TASK-045 — одноразовые коды и секреты устройств: окончательный план

Базовый план: `PLAN.md` (planner). Эталон: `scratch/planner/task045.patch` против `main` **7748a43**. Поверх него три правки ревью: `scratch/plan-reviewer-2/amendments.patch`. Итоговые хеши 18 файлов (LF-байты): `scratch/plan-reviewer-2/hashes_final.txt`, проверка `bash maw/tasks/in_progress/TASK-045/scratch/plan-reviewer-2/verify_final.sh`. Все 18 файлов дают OK на свежем `git archive 7748a43` после двух `git apply --ignore-whitespace`. Это проверено 2026-09-26.

## 1. Summary

Каждое устройство получает свой секрет вместо общего `CCTG_HUB_SECRET`. Одноразовый код `XXXX-XXXX-XXXX-XXXX` (16 знаков Crockford base32, 80 бит из CSPRNG, живёт 10 минут, работает один раз) выпускает `cctg hub code` рядом с hub (`docker compose exec hub cctg hub code`) или `install.sh --hub`. Код лежит в `<state>/join/<sha256(code)>.json`, внутри только срок. Клиент `cctg join` (его зовёт `install.sh --join КОД`, код передаётся через окружение) отправляет код в `POST /v1/join` на адрес хуков: по TLS с pin или на loopback. В ответ приходит секрет `cctgd_<id>_<64 hex>`, клиент пишет его в `~/.cctg/device.env` строкой `CCTG_HUB_SECRET`. Поэтому агент, хук и протокол линка не меняются, `wire::VERSION` тот же. Hub хранит sha256 секрета в `<state>/devices.json` и проверяет через `Devices::check` (сначала общий секрет, если он включён, потом устройство по публичному id и сравнение хеша через `subtle`). `/devices` в General показывает список устройств с кнопкой «Отозвать» и подтверждением. Отзыв сразу закрывает линки агентов устройства через `watch` и обрывает его ждущие хуки. Следующий хук этого устройства получает 401. Общий секрет работает, пока `CCTG_SHARED_SECRET` не `off`, так миграция проходит без поломки.

## 2. Implementation steps

Порядок: применить эталон, потом правки ревью. Код руками не переписывать.

0. Ветка `feature/device-enrollment` от актуального `main`. Если `main` всё ещё `7748a43` (TASK-053 не слита), выполнить:
   ```
   git apply --ignore-whitespace maw/tasks/in_progress/TASK-045/scratch/planner/task045.patch
   git apply --ignore-whitespace maw/tasks/in_progress/TASK-045/scratch/plan-reviewer-2/amendments.patch
   bash maw/tasks/in_progress/TASK-045/scratch/plan-reviewer-2/verify_final.sh   # 18 x OK
   ```
   Если `main` ушёл вперёд (слита TASK-053): применить оба патча с `--3way`, разрешить конфликты по `PLAN.md` §5. Там места не пересекаются: `wire.rs` (вставка после `decode_permission`), тесты в конце `ingress.rs`, `install.sh` вне `write_claude_files`, `install_e2e.rs` без `EVENTS`. После этого `verify_final.sh` даёт MISMATCH только на файлах, которые изменила TASK-053. Для них проверяется глазами, что наши куски на месте.

1. Эталон (`task045.patch`) по файлам, как в `PLAN.md` §3–§4:
   - `crates/cctg/src/hub/devices.rs` (новый). `Devices` (Arc): `open`, `check`, `is_active`, `subscribe`, `join`, `enroll`, `revoke`, `list`, `name`. Коды: `mint_code`, `take_code`, `sweep`. Плюс `clean_name`, `normalize_code`, `secret_device_id`. Запись temp + fsync + rename, 0600 на Unix.
   - `crates/cctg/src/wire.rs`: `JOIN_PATH`, `MAX_JOIN_BODY = 1024`, `JoinPost`, `JoinAnswer` (`Debug` без кода и секрета), `decode_join`.
   - `crates/cctg/src/hub/ingress.rs`: `auth: impl Into<Devices>` во всех пяти `serve_*`. Подписка на `watch` до проверки секрета, ветка `revoked` в цикле агента, четвёртая ветка в `wait_for`. `Route::Join`: без `Authorization`, тело ≤1 КиБ. `join_request`: 200 + `JoinAnswer`, одинаковый 403 после `AUTH_FAIL_DELAY`, 503. `Status::Forbidden`.
   - `crates/cctg/src/hub/config.rs`: `SHARED_VAR = "CCTG_SHARED_SECRET"` (`on` по умолчанию, `off`, иначе ошибка без эха значения), `Config.shared_secret`, `state_dir(env_file)`.
   - `crates/cctg/src/hub/roster.rs` (новый): воркер `/devices` и кнопки `dev:r:<id>`, `dev:y:<id>`, `dev:n`.
   - `crates/cctg/src/hub/mod.rs`: модули, `route_inbound(commands, roster, control, bot_id)`, в `run` вызовы `Devices::open` и `roster::serve`, `mint_code(env_file)`.
   - `crates/cctg/src/join.rs` (новый), `lib.rs` (`pub mod join;`), `main.rs` (`hub code`, `join [КОД]`, `--env-file` global), `doctor.rs` (строка «this device's own / shared»).
   - Тесты: `crates/cctg/tests/join_e2e.rs` (новый), `install_e2e.rs` (`a_device_joins_with_a_code`, кривой код, `--hub` с `exec -T hub cctg hub code`), `hub_reads_no_files.rs` (`devices.rs` в `OWN_STATE`, починка `code_only` для `""`).
   - `install.sh` (`--join`, `CCTG_JOIN_CODE`, `check_code`, `join_hub`, `--hub` печатает строку с `--join`, общий секрет больше не печатает). Документы: `docs/remote-hub.md`, `deploy/hub.env.example`, `README.md`, `CLAUDE.md`.

2. **Правка A1, `crates/cctg/src/hub/ingress.rs`, `hook_request`, ветка `Ok(Ok((Route::Join, body, _)))`.** Убрать `drop(permit);` и поставить комментарий:
   ```rust
   Ok(Ok((Route::Join, body, _))) => {
       // The request place stays taken, also through the pause of a
       // refused code (as for a wrong secret).
       return join_request(stream, peer, &body, devices, &gate).await;
   }
   ```
   Причина: в эталоне отвергнутый код отпускает место запроса хуков (`MAX_HOOK_REQUESTS = 64`) до паузы 250 мс. Путь 401 это место держит (`// No fast guessing; the request place stays taken meanwhile.`). Без правки неаутентифицированные join-запросы обходят лимит 64 мест TASK-035, а `docs/remote-hub.md` («в тех же местах хуков, 64 всего») описывает поведение, которого в коде нет. Доказательство в Review notes, п. 1.
   Там же новый тест `a_refused_join_keeps_its_request_place_through_the_pause` (`#[cfg(not(target_os = "macos"))]`: на macOS настроен только `127.0.0.1`). Он шлёт 64 join с плохим кодом с разных `127.0.3.x` через `connect_from`, ждёт 100 мс, потом 65-е соединение с `127.0.4.2` должно быть закрыто сразу без ответа (`closed_at_once`), а все 64 получают `HTTP/1.1 403`. Текст теста в `amendments.patch`.

3. **Правка A2, `crates/cctg/src/doctor.rs`, ветка `Err(PostError::Status(401))`.** В эталоне в строку попали 22 лишних пробела («a device                      secret»). Правильный вид:
   ```rust
   format!(
       "hooks {hooks}: the hub rejected the secret (compare CCTG_HUB_SECRET; a device \
        secret may have been revoked: join again with a new code, cctg join)"
   ),
   ```
   `\` в конце строки литерала съедает перевод строки и ведущие пробелы. Текст выходит одной строкой с одним пробелом.

4. **Правка A3, `docs/remote-hub.md`.** Три добавления, точный текст в `amendments.patch`:
   - В пункте «Код» после «(ему нужен только `CCTG_STATE_DIR`).»: `cctg hub code` запускать от того же пользователя, что и hub (`docker compose exec` без `-u`, без `sudo`). Файл кода другого пользователя с правами 0600 hub не прочитает, и код отвергается как неверный.
   - «Миграция», шаг 3: строка «последний вход с общим секретом» обновляется только при новом входе. Уже подключённый агент старой сессии её не двигает, поэтому перед `off` надо перезапустить сессии claude, начатые до `cctg join`.
   - После «Откат:» новый абзац. Откат образа hub на релиз до TASK-045 не знает `devices.json`, и машины со своим секретом получат отказ. Им снова нужен общий секрет: `install.sh` с `CCTG_HUB_SECRET` или `--secret-file`.

5. **Сборка и проверка перед коммитом** (общий target, `-j 1`):
   ```
   export CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0
   touch crates/cctg/src/lib.rs crates/cctg/src/main.rs \
         crates/cctg/tests/join_e2e.rs crates/cctg/tests/install_e2e.rs crates/cctg/tests/hub_reads_no_files.rs
   cargo fmt --all -- --check
   cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings
   cargo test -j 1 --workspace --locked --no-fail-fast
   ```
   Трогать надо и файлы тестов, не только `lib.rs`/`main.rs`. Иначе cargo может запустить тестовый бинарь, собранный из другого дерева с тем же target. Так было на ревью: `install_e2e` прогнал 10 тестов без нового `a_device_joins_with_a_code`. В выводе обязательно должны быть: `a_device_joins_with_a_code`, `a_device_joins_with_a_code_and_is_out_after_a_revoke`, `a_refused_join_keeps_its_request_place_through_the_pause` (кроме macOS), `a_join_code_buys_one_device_secret_and_nothing_else`, `a_revoked_device_loses_its_link_and_its_hooks_at_once`, `a_waiting_hook_of_a_revoked_device_is_dropped_unanswered`, `with_the_shared_secret_off_only_devices_get_in`, `the_list_asks_before_it_revokes_and_the_revoke_cuts_the_device`, `the_guard_sees_a_file_read`. Нет хотя бы одного — значит прогнан чужой бинарь, повторить после `touch`. Падения `files_e2e`/`question_hook_e2e` под нагрузкой параллельной сборки перепрогнать отдельным бинарём (`-p cctg --test <имя>`).

6. Коммиты без trailer'ов «Generated with» и «Co-Authored-By» (правило проекта).

## 3. Test plan

Автоматические тесты (все есть в эталоне плюс A1). На ревью они прогнаны на Windows во `%TEMP%`-копии и зелёные.

| Что | Тест | Ожидание |
|---|---|---|
| Код одноразовый, истекает, мусор | `devices::a_code_is_good_once_and_only_until_it_expires`, `codes_are_crockford_groups_and_normalize` | второй take и просроченный дают `Refused`; `O→0`, `I/L→1` |
| В `join/` нет кода | там же | ни в именах, ни в содержимом |
| Лимит кодов | `minting_stops_at_the_cap_and_expired_codes_free_places` | 33-й `MintError::Full`, просроченные освобождают |
| Хранится только хеш | `a_device_secret_gets_in_and_only_its_hash_is_kept` | секрет пускает, чужой хвост с тем же id нет, в `devices.json` нет секрета |
| Битый/чужой `devices.json` | `a_full_book_refuses_and_a_bad_file_stops_the_start` | `open` Err без цитаты файла |
| HTTP join | `ingress::a_join_code_buys_one_device_secret_and_nothing_else` | 200 + секрет; повтор, выдуманный и просроченный: 403 без тела, не раньше 250 мс; 413 для тела >1 КиБ; 400 для мусора; `Authorization` игнорируется |
| Место запроса держится (A1) | `ingress::a_refused_join_keeps_its_request_place_through_the_pause` | 65-е соединение закрыто сразу; 64 ответа 403. Проверено: без A1 тест падает, с A1 проходит 6 из 6 |
| Отзыв рвёт линк и хуки | `a_revoked_device_loses_its_link_and_its_hooks_at_once`, `a_waiting_hook_of_a_revoked_device_is_dropped_unanswered` | `Disconnected`; хук 401; новый `hello` `Rejected(Auth)`; линк общего секрета жив; ждущий хук закрыт без ответа |
| `off` | `with_the_shared_secret_off_only_devices_get_in`, `config::the_shared_secret_is_on_unless_turned_off` | общий секрет не пускает ни агент, ни хук; мусор в `CCTG_SHARED_SECRET` без эха |
| `/devices` | `roster::*`, `hub::tests::messages_go_to_the_slot_actor_and_commands_do_not` | в General воркер, в теме сессия; подтверждение, отмена, повторный отзыв «уже нет»; 32 устройства × 32 символа ≤4096 |
| Клиент | `join::*` | обмен через настоящий `serve_hooks`; удалённый hub без pin в сеть не идёт; строка секрета заменяется, остальные строки на месте; 0600 на Unix; строгий разбор ответа |
| CLI | `main::tests::parses_all_subcommands` | `hub code`, `hub --env-file x code`, `join КОД`, `join` |
| e2e бинарём | `join_e2e` | TLS + pin; `hub code` печатает только код; `device.env` получил `cctgd_…`; повтор кода: выход 1 и текст отказа; отзыв даёт в `doctor` «rejected the secret»; в логах hub нет кода, секрета и общего секрета |
| Установщик | `install_e2e::a_device_joins_with_a_code`, `missing_or_bad_settings…`, `a_hub_is_set_up_with_docker_compose` | `--join` без печати секрета и кода; повтор без кода сохраняет секрет; потраченный код ничего не меняет; `--hub` печатает `--join`, общий секрет ни разу |
| Сторож | `hub_reads_no_files` (все 3) | `devices.rs` своё состояние; `""` больше не ломает сканер |
| Остальное | весь `cargo test --workspace` | зелёный (CI: Ubuntu, Windows, macOS) |

Живая проверка после выкатки (оркестратор, не implementer, без окон и без Telegram из pipeline):
1. Образ нового релиза на сервере. `docker compose exec hub cctg hub code` печатает `XXXX-XXXX-XXXX-XXXX`.
2. На Mac или в Linux-контейнере `~/.cctg/bin/cctg join КОД`. Потом `cctg doctor` показывает «secret: set, this device's own (device …)».
3. `/devices` в General: устройство есть, «на связи N мин назад».
4. «Отозвать» → «Да». На тестовой машине `cctg doctor` пишет «rejected the secret», сессия теряет агента.
5. Ручной `~/.cctg` Windows-машины перевести тем же `cctg join`.

## 4. Rollout notes

- **Новая настройка hub** `CCTG_SHARED_SECRET` (`on` по умолчанию | `off`). Свежий `install.sh --hub` оставляет `on` и по-прежнему создаёт общий секрет в `hub.env`, но не печатает его (решение оркестратора 1). При `off` hub стартует и без `CCTG_HUB_SECRET`.
- **Новые файлы состояния** в `<CCTG_STATE_DIR>` (`/data` в Docker): `devices.json` (0600 на Unix) и `join/`. Если `devices.json` битый или чужой версии, hub не стартует (как с `registry.json`). Сообщение называет файл и не цитирует его.
- **Совместимость.** Протокол линка и `wire::VERSION` не меняются. Старые клиенты с общим секретом работают, пока `on`. Старые бинарники клиента с секретом устройства тоже работают (79 видимых ASCII проходят `Secret::parse`). Новый `cctg join` против старого hub получает 404 и сообщение «hub older than join codes».
- **Откат.** Для настройки: `CCTG_SHARED_SECRET=on` и перезапуск. Откат образа на релиз до TASK-045 выключает устройства со своим секретом, им снова нужен общий секрет (A3).
- **Миграция** описана в `docs/remote-hub.md` «Устройства и коды / Миграция». Шаги: новый hub (`on`), `cctg join` на каждой машине, перезапуск старых сессий claude (A3), `off`, перезапуск hub.
- **Выпуск кода другим процессом.** `cctg hub code` должен видеть тот же `CCTG_STATE_DIR` и работать от того же пользователя, что и hub. В образе это `USER 10001`, `WORKDIR /data`, `ENV CCTG_STATE_DIR=/data`: exec наследует всё это, проверено по `Dockerfile`.
- **Релиз.** `install.sh` тега ставит бинарник того же тега, поэтому `--join` в строке клиента требует релиза с TASK-045 на обеих сторонах.
- Решения оркестратора: 1 общий секрет `on`; 2 подтверждение отзыва остаётся; 3 без автоотзыва при повторном join; 4 привязка секрета к host вне задачи; 5 без `--minutes`; 6 `/join` в General уходит в TASK-046.

## 5. Review notes

Вердикт: **PLAN_FINAL**. Эталон годен, три точечные правки A1–A3 и уточнение шага сборки.

**Опровержение (сделано до оценки).** Самый конкретный контрпример к плану: «`docker compose exec hub cctg hub code` пишет код туда, откуда hub его не возьмёт (другой каталог состояния, другой пользователь, чужой `./.env`)». Проверено по `Dockerfile` и `deploy/compose.yml`: `USER 10001`, `WORKDIR /data`, `ENV CCTG_STATE_DIR=/data`. `docker compose exec` запускает команду без entrypoint с окружением контейнера (включая `env_file: hub.env`). `COMPOSE_FILE` лежит в `.env` папки hub, так что `exec` видит тот же проект. `config::state_dir` читает `./.env` только если он есть, а в `/data` его нет. **Контрпример не подтвердился.** Остался один крайний случай: `exec -u root` или `sudo` создаёт файл, который hub не прочитает. Это ушло в документацию (A3).

Найденные дефекты (доказательства в `scratch/plan-reviewer-2/`):

1. **Отвергнутый join отпускал место запроса хуков до паузы 250 мс (средний).** В `hook_request` ветка `Route::Join` делала `drop(permit)` до `join_request`, а путь 401 держит место всю паузу. Проба `join_place_probe.rs.txt` (две волны по 64 запроса с разных `127.0.x.y`, вторая идёт, пока первая спит) показала: join с плохим кодом `64/64` и `64/64`, хук с плохим секретом `64/64` и `0/64`. Значит лимит 64 мест TASK-035 для `/v1/join` не действовал, и аргумент «≈10^5 попыток за 10 минут» из `PLAN.md` §2 опирался на лимит, которого нет. Перебор 2^80 всё равно нереален, так что это в первую очередь ресурсная дыра до аутентификации и расхождение с документацией. Исправлено A1. Новый тест падает на эталоне и проходит с правкой (6 прогонов подряд). clippy `-D warnings` и fmt чистые.
2. **Сломанная подсказка `cctg doctor` на 401 (низкий).** В литерале 22 пробела подряд, пользователь увидел бы «a device                      secret». Исправлено A2.
3. **Документация миграции неполная (низкий).** `shared_seen` двигается только при новом входе. Агент уже идущей сессии с общим секретом его не обновляет, поэтому «давно не меняется» не значит «никто не пользуется», и `off` отрежет такие сессии. Кроме того, не был описан откат образа (он выключает устройства со своим секретом) и то, что `cctg hub code` надо запускать от пользователя hub. Всё это закрыто A3.
4. **Шаг сборки (процесс).** «touch lib.rs» из `PLAN.md` мало. На ревью `install_e2e` из общего target запустил бинарь без нового теста (10 тестов вместо 11, строки `a_device_joins_with_a_code` в exe нет), пока не был тронут `tests/install_e2e.rs`. Шаг 5 теперь трогает и файлы тестов и перечисляет обязательные имена. Предложение для project context записано в `PCTX_PROPOSALS.md`.

Проверено и оставлено как есть:
- Replay и гонка кода: `take_code` читает срок, потом `remove_file`. Из двух гонящихся выигрывает один (на Windows второй получает `NotFound` или `AccessDenied`). Код, не прошедший `normalize_code`, отсекается до файловой системы.
- Утечки по времени: общий секрет сравнивается через `Secret::matches` (ct), устройство через `ct_eq` хешей. Утекает только существование публичного id. Все отказы по коду дают одинаковый 403 после 250 мс. Разница «просрочен» и «не было» в единицах миллисекунд (sweep), и чтобы её увидеть, нужно уже знать код.
- Секреты и коды в логах, stdout и argv: `JoinPost`/`JoinAnswer`/`Enrolled`/`Devices` скрывают их в `Debug`. В `join_e2e` есть негативные проверки логов. `cctg join` печатает только имя и id. `install.sh` передаёт код через окружение, а не через argv. Ручной `cctg join КОД` кладёт код в argv на время одного обмена, так же как строка `install.sh … --join КОД` уже делает. Код одноразовый и живёт 10 минут.
- Права: `devices.json`, коды и `device.env` создаются через temp с 0600 на Unix. На Windows права наследуются от папки, как у `device.env` до задачи.
- `/v1/join` до аутентификации: маршрут разбирается до заголовков, тело ≤1 КиБ, дедлайн 2 с, 16 недочитанных на IP, после A1 64 места. `linger` сохраняет ранние 413/400.
- Отзыв: подписка на `watch` до проверки секрета, поэтому отзыв между `hello` и `register` не теряется. `watch::Receiver::changed` безопасен при отмене в `select!`. `send_modify` работает и без получателей.
- Перезапуск hub: `devices.json` и `join/` на диске. Время последнего входа только в памяти, это описано.
- `tests/hub_reads_no_files.rs`: починка `code_only` верная. Старый код пропускал первый символ каждой обычной строки, и `""` съедал закрывающую кавычку. Сторож от починки стал строже, все 3 теста зелёные.
- Оставлено сознательно (риски из `PLAN.md` §6 плюс эти): код тратится и при `JoinError::Full`/`Io`. Проверка ёмкости до траты давала бы 503 на неверный код полного hub, то есть подсказку без кода. `enroll`/`revoke` держат `std::Mutex` на время fsync, и `check` на потоках рантайма может коротко ждать. Для ≤32 устройств и редких join/revoke это терпимо. Если ответ join потерян, остаётся устройство-сирота, оно видно в `/devices` и отзывается.

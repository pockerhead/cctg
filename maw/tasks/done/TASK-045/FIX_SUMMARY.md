# TASK-045 — FIX_SUMMARY

Ветка `feature/join-codes`, worktree `C:/Users/user/dev/cctg-045`, поверх `de8281d`. Один коммит `fix: ... (TASK-045)`.

Preflight: самое опасное предложение ревью, если его сделать дословно, это находка 2, «отказывать, если в state dir нет `registry.json`/`offset`». Проверил: `install.sh --hub` зовёт `cctg hub code` сразу после старта hub (`wait_hub` ждёт только строку лога). Offset пишется после первого батча `getUpdates`, registry пишет сейвер асинхронно. Такая проверка давала бы гонку при установке. Её не брал, сделал другой признак (ниже).

## 1. Fixed

- **Находка 1 (хук, отозванный между заголовками и телом).** `hub/ingress.rs`, `hook_request`: новая первая ветка `Route::Hook | Route::Ping` с `Some(who)`, у которого `!devices.is_active(&who)` → 401, событие в hub не уходит. Тест `a_hook_revoked_between_its_header_and_its_body_is_refused`: шлёт заголовки и часть тела, ждёт 200 мс, отзывает устройство, дошлёт хвост. Ожидает 401 и отсутствие события. Мутация (guard выключен): тест падает с `HTTP/1.1 204`.
- **Находка 2 (`cctg hub code` не туда).** Каталог состояния разрешается прежней функцией `config::state_dir`: env, потом `--env-file` или `./.env`, как в `Config::load` у hub. Признак «здесь стартовал hub» это `<state>/join/`. Его создаёт `Devices::open` при старте hub, раньше любой сети. `devices::mint_code` больше не делает `create_dir_all`. Нет `join/` → `MintError::NoHub`, и `hub::mint_code` пишет в stderr «no hub has started with the state directory <абсолютный путь> (run cctg hub code where the hub runs, with its CCTG_STATE_DIR or --env-file)». В stdout ничего, выход 1. Тесты: `devices::a_code_is_minted_only_where_a_hub_keeps_its_state` (ничего не создаётся) и в `join_e2e` прогон настоящего бинаря против `~/.cctg` устройства и несуществующей папки. Добавлено предложение в `docs/remote-hub.md`.
- **Находка 3 (срок кода).** `devices.rs`: `expiry` заменён на `deadline(path, now)` = `min(expires, mtime файла + 10 мин)`. Если срок больше чем `now + 10 мин` (часы убежали вперёд, файл написан руками), он считается истёкшим. Используется и в `take_code`, и в `sweep`. Тест `a_code_lives_at_most_ten_minutes_whatever_its_file_says`: `expires = u64::MAX` + взят вовремя → ok; тот же код, взятый через 10 мин + 2 с → отказ; mtime на 10 мин + 2 с в прошлом → отказ; код, выпущенный «из будущего» → отказ.
- **Найдено по ходу (серьёзнее находок ревью).** Тест на гонку двух `take_code` (его просило ревью) падал 3 из 3 с `left: 2`: на Windows два параллельных `remove_file` одного файла оба возвращают Ok, потому что каждый удаляет через свой handle. Значит, один код мог выдать два устройства. Утверждение «из двух попыток проходит одна» в IMPL_REVIEW и PLAN_FINAL на Windows неверно. Исправил так: `take_code` берёт статический `Mutex` на весь процесс (коды из своего state dir берёт один hub). После этого 5 из 5 прогонов зелёные. Тест `of_two_racing_takes_of_one_code_one_wins`, 20 раундов с `Barrier`.
- **Nit: `join.rs` проверяет ответ через `secret_device_id`.** Вместо `Secret::parse` теперь `secret_device_id(&answer.secret) == Some(&answer.device_id)`. Секрет с `'`, общий секрет и секрет чужого id дают `BadAnswer`. Тест `only_a_device_secret_of_the_named_device_is_taken` с фейковым hub.
- **Nit: паника на битом `joined`.** `Devices::open` отвергает запись, у которой `UNIX_EPOCH.checked_add(joined)` = None (`LoadError::Invalid`). `list` тоже использует `checked_add` с fallback. Тест в `a_full_book_refuses_and_a_bad_file_stops_the_start` (`joined = u64::MAX`).
- **Nit: код в install.sh вводился с эхом.** Интерактивный запрос кода теперь идёт через существующий `read_hidden` (`stty -g` / `stty -echo`, восстановление там же и в `cleanup` по EXIT/INT/TERM, POSIX sh, Git Bash). `sh -n install.sh` ok.
- **Nit: «на связи».** `roster.rs`: «последний вход N назад» / «с запуска hub не входило». Так совпадает со строкой общего секрета и с `docs/remote-hub.md`.
- **Недостающие тесты:** права 0600 на `devices.json` и файлы кодов (`#[cfg(unix)] the_device_list_and_codes_are_the_owners_alone`), гонка `take_code` (выше), находки 1 и 2 (выше).

## 2. Skipped

- **Предложенный в находке 2 признак `registry.json`/`offset`**: гонка с `install.sh --hub` (см. preflight). Заменён на `join/`, который создаёт `Devices::open`.
- **Вариант находки 3 `expires <= now + TTL + 60` сам по себе**: не ограничивает файл, записанный давно с большим `expires`. Взят вариант с mtime плюс эта же граница.
- **Nit про 0600 и оставшийся temp с широкими правами**: для этого нужен уже существующий `.tmp` с чужими правами в собственном каталоге hub. Это вне заданного списка, не трогал.
- **Nit про переиспользование id после отзыва (2^-32)**: вероятность пренебрежимая, в заданный список не входит.
- **Nit про fsync каталога**: так же устроены `registry.rs` и `offset.rs`, ревью само предлагает не трогать.
- **Тесты `install.sh --join` на режим файла и `hub::run` с `off` без секрета**: не дешёвые (нужен Unix или живой Bot API), не добавлял.

## 3. Test results

Общий target `C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, перед прогоном touch `lib.rs`, `main.rs`, `tests/*.rs`.

- `cargo fmt --all -- --check`: OK.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: `Finished`, 0 warnings (`scratch/fixer-clippy.txt`).
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/fixer-test.txt`): **915 passed, 0 failed, 3 ignored**, 43 бинарника. Перепрогонять ничего не пришлось. Новые и обязательные тесты в выводе все `ok`, в том числе `a_device_joins_with_a_code`, `a_device_joins_with_a_code_and_is_out_after_a_revoke`, `a_refused_join_keeps_its_request_place_through_the_pause`, `the_guard_sees_a_file_read`.

# TASK-032 QA_REPORT

Проверялось: ветка `feature/files`, HEAD `59d9769` (код `96f1e4b` + фикс `a30815a`), `git diff 6532c7f HEAD -- crates docs`.

## Disconfirmation

Контрпример, который делал бы реализацию неверной: "`send_file` на пути, который `metadata` считает обычным файлом, а чтение которого никогда не кончается (именованный канал Windows `\\.\pipe\...`), навсегда занимает единственный воркер отправки агента". Эту проверку в code review не пропустил классификатор прав (запись `dead_end` в log.jsonl), так что она оставалась непроверенной.

Проверил. Контрпример **подтвердился**: `std::fs::metadata(r"\\.\pipe\x")` для живого канала возвращает `is_file() == true, len 0`. После этого `files::read_upload` висит в `read_to_end` (тест `qa_read_upload_on_a_named_pipe` за 5 с не дождался ответа). Подробности в разделе Bugs, серьёзность low.

Второй контрпример, из главной находки ревью ("файл, который скачивается в момент SessionEnd, уходит завершённой сессии и получает 👀"), я проверил по коду `on_fetched` (`slots.rs:1896-1980`). Ветка `Handed` засчитывается, только если `live_agent(slot)` имеет тот же conn, иначе сообщение остаётся в слоте и вызывается `flush`. Тест `a_file_downloading_when_its_session_ends_stays_in_the_slot` проходит. Фикс на месте.

## 1. Environment

- Прямой прогон cargo, без docker-compose и без dev-сервера. Внешние зависимости (Telegram Bot API, Claude Code) в тестах заменены фейками самого проекта: фейковый HTTP Bot API и `serve_channel` в роли Claude Code в `files_e2e`. Настоящий Telegram, `.env`, интерактивный claude, `~/.cctg`, живой hub и supervisor я не трогал.
- Все сборки шли с `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0` и `-j 1`, по одному cargo за раз. Target не удалялся.
- Воспроизвести, из корня репозитория:
  - `cargo fmt --all -- --check`
  - `cargo clippy -j 1 --workspace --all-targets -- -D warnings`
  - `cargo test -j 1 --workspace --no-fail-fast`
  - `cargo test -j 1 -p cctg --test files_e2e --test update_e2e`
  - Свои тесты: `git archive HEAD crates Cargo.toml Cargo.lock` в `%TEMP%/qa032`, `scratch/qa/qa_files.rs.txt` скопировать в `crates/cctg/tests/qa_files.rs`, затем `cargo test -j 1 -p cctg --test qa_files -- --nocapture`. Копия после прогона удалена.
- Сервисов и контейнеров я не запускал, останавливать нечего.

## 2. Test results

| Прогон | Результат | Вывод |
|---|---|---|
| `cargo fmt --all -- --check` | чисто, rc=0 | `scratch/qa/fmt.txt` |
| `cargo clippy --workspace --all-targets -D warnings` | чисто, rc=0 | `scratch/qa/clippy.txt` |
| `cargo test --workspace --no-fail-fast` | **711 passed, 0 failed, 3 ignored**, rc=0; `run_e2e: ok`, `supervise_e2e: ok`; `cctg --lib` 549 passed, 1 ignored | `scratch/qa/workspace_test.txt` |
| `files_e2e` (`files_go_both_ways_and_never_reach_the_logs`) | 1 passed | `scratch/qa/e2e_test.txt` |
| `update_e2e` (`a_new_binary_is_taken_without_losing_a_line`) | 1 passed | `scratch/qa/e2e_test.txt` |

Числа совпадают с FIX_SUMMARY (711/0/3). Артефакт общего target с purity на этот раз не проявился.

Свои тесты (`scratch/qa/qa_files.rs.txt`, вывод в `scratch/qa/qa_files.out.txt`):

| Тест | Что проверяет | Результат |
|---|---|---|
| `qa_clean_name_edges` | `..`, `../../etc/passwd`, `..\..\Windows\win.ini`, `C:evil.txt`, `a.txt::$DATA` (ADS), ` .hidden. `, RLO `\u{202E}`, пустое, `/`, `dir/`; 200 кириллических символов + `.pdf`; длинное "расширение"; хвост из точек и пробелов после обрезки | PASS: без `/ \ :`, ≤120 байт, расширение сохраняется, граница символа не рвётся |
| `qa_save_never_overwrites_and_keeps_the_gitignore` | два `same.txt` дают `same-2.txt`, два `noext` дают `noext-2`, содержимое не перезаписано; `.cctg/inbox/.gitignore` = `*\n`; если `.cctg` файл, а не папка, сохранение уходит в `<temp>/cctg-inbox` | PASS |
| `qa_read_upload_edges` | каталог и отсутствующий путь дают `NotFile`, 0 байт `Empty`, ровно 50 MiB принимается, 50 MiB + 1 даёт `TooBig` | PASS |
| `qa_read_upload_on_a_named_pipe` | `read_upload(r"\\.\pipe\...")` при живом сервере канала | **FAIL**: висит больше 5 с (баг 1) |
| `qa_pipe_where` (диагностика) | `metadata` канала | `Ok((true, 0))`: `is_file() == true` |

## 3. Acceptance criteria

| Критерий | Тест / доказательство | Результат |
|---|---|---|
| Документ и фото из темы становятся файлами на машине сессии, сессия получает путь и подпись; >20 MB даёт понятный ответ | `files_e2e::files_go_both_ways_and_never_reach_the_logs`: документ и фото сохраняются в inbox, в channel-сообщении есть `file_path` и подпись, при >20 MB тема получает `TOO_BIG_NOTICE` (строка 478). Unit-тесты `agent::tests::a_file_saved_off_the_loop_still_reaches_claude_before_the_messages_after_it` и `a_file_from_the_topic_is_saved...`. Мои `qa_save_*` и `qa_clean_name_edges`. Порог 20 MiB проверяется в трёх местах: анонс (`slots.rs:1640`), `getFile.file_size` (`fetch.rs:55`), `download` (Content-Length и поток; тест `a_download_stops_at_its_limit_by_length_or_by_bytes_and_a_refusal_is_told`) | PASS |
| `send_file` шлёт картинку фото, остальное документом, в тему своей сессии; путь не к файлу или >50 MB даёт ошибку инструмента | `files_e2e`: PNG уходит `sendPhoto` с `message_thread_id` 100, `notes.txt` уходит `sendDocument`, `REFUSE-PHOTO` уходит документом, 50 MiB + 1 даёт ошибку инструмента. `scheduler`/`api`: `only_a_refused_picture_goes_again_as_a_document`. Мой `qa_read_upload_edges` (каталог, отсутствующий путь, 0 байт, граница 50 MiB). Исключение: именованный канал (баг 1) | PASS (с low-багом 1) |
| Передача по agent-линку чанками; старый агент без capability получает понятную заглушку; VERSION не меняется | `wire.rs:38` `VERSION: u32 = 1`, в диффе `6532c7f..HEAD` строк с `VERSION` нет. `wire::tests::files_stay_compatible_with_version_one_peers`. `files_e2e`: агент без `files` получает подпись текстом, тема получает `OLD_AGENT_NOTICE` (строки 595-608). Чанки: отправитель режет по 64 KiB, приёмник берёт до 256 KiB (`an_assembly_takes_only_the_next_piece...`, со строкой < MAX_LINE/2). Кадры идут через настоящий `serve_agents` | PASS |
| Мёртвый слот хранит файл ссылкой и доставляет его при оживлении | `files_e2e`: в `registry.json` лежит `"file_id": "pic"` без байтов, после resume файл доставлен с подписью (строки 542-560). Unit `a_dead_slot_keeps_a_file...` и `a_file_downloading_when_its_session_ends_stays_in_the_slot` | PASS |
| Тесты через настоящий `serve_agents` и фейковый Bot API; содержимое и имена файлов не в логах | `files_e2e` поднимает настоящие `BotApi` + `Scheduler` + `Slots` + `serve_agents` + `agent::spawn`. Логи TRACE (с reqwest/hyper) проверены на маркер в именах, подписях, содержимом и путях и на токен бота (строки 615-628). Новые `info!/warn!` в `fetch.rs`/`slots.rs`/`agent.rs` я прочитал: в них только kind, size, ordinal, conn, outcome, `%error` от `ApiError` без URL | PASS |
| Existing tests pass | workspace 711/0/3, fmt и clippy чисто | PASS |

## Находки ревью: проверка фиксов по коду

| Находка | Что в коде | Статус |
|---|---|---|
| major-1: файл, который скачивается в момент SessionEnd | `Fetching{transfer_id,message_id,conn}`. `on_fetched` при `Handed` на conn, который уже не `live_agent`, оставляет сообщение и вызывает `flush` (`slots.rs:1908-1920`). Тест проходит, мутация M1 KILLED | Fixed. Цена: повторная доставка (at-least-once), если умирающий агент успел сохранить файл |
| minor-2: переполнение во время скачивания | `Buffer::push(msg, keep_front)` удаляет `remove(1)`, когда `fetching` содержит слот (`slots.rs:1694`, `buffer.rs`). Тесты buffer и slots проходят | Fixed |
| minor-3: большие строки чанков и бесконечные повторы | `CHUNK` 64 KiB у отправителя, `MAX_CHUNK` 256 KiB у приёмника (совместимо с агентами `96f1e4b`). `MAX_LINK_LOSSES = 3` + `LINK_LOST_NOTICE`, счётчик сбрасывается любым другим исходом | Fixed |
| minor-4: фолбэк `sendPhoto` на любой 400 | `ApiError::is_photo_refusal` (400 и `PHOTO`/`IMAGE` без учёта регистра), в `scheduler.rs:232` фолбэк только в этом случае | Fixed |
| minor-5: сохранение блокирует MCP-цикл | Сохранение идёт в `tokio::spawn(deliver(..))`, результат возвращается веткой `select!`, события hub не читаются, пока `saving.is_some()` (`agent.rs:714-729`). stdin и ответы инструментов обрабатываются | Fixed. Цена задокументирована: вердикт и ответ на `file_offer` ждут одно сохранение |
| Missing coverage `BotApi::download` | тест с сырым HTTP-сервером: лимит по Content-Length, лимит по потоку, 404 без токена | Fixed |
| Именованный канал в `read_upload` (не проверено ревью) | Проверено здесь: висит | **Не исправлено**, баг 1 |
| Рестарт hub во время скачивания, двусторонняя передача | Не покрыто, fixer пропустил осознанно | Открыто (покрытие) |

## 4. Bugs found

### 1. low: `send_file` на именованном канале Windows навсегда вешает отправку файлов этого агента

- **Где:** `crates/cctg/src/files.rs:328-345` (`read_upload`), вызов в `agent.rs` `upload` через `spawn_blocking`, единственный воркер `spawn_sender`.
- **Воспроизведение:** создать сервер канала `\\.\pipe\x` (например, `tokio::net::windows::named_pipe::ServerOptions`, несколько экземпляров) и вызвать `files::read_upload(Path::new(r"\\.\pipe\x"))`. Тест: `scratch/qa/qa_files.rs.txt::qa_read_upload_on_a_named_pipe`.
- **Ожидалось:** `Err(UploadError::NotFile)`: "путь не к обычному файлу даёт ошибку инструмента".
- **Фактически:** `metadata` возвращает `is_file() == true, len() == 0` (`qa_pipe_where`: `Ok((true, 0))`), и `read_to_end` ждёт данных от сервера канала без таймаута. Воркер отправки один на агента. Этот вызов `send_file` не получает ответа (Claude Code через 2 минуты уводит его в фон), следующие 4 встают в очередь навсегда, дальше все получают "Too many files are being sent". Так продолжается до перезапуска агента.
- **Условие:** Claude должен явно вызвать `send_file` с путём канала (`\\.\pipe\...`) при живом сервере. Сессия пользователя от этого не падает, text/permission relay работают. Поэтому серьёзность low.
- **Правка (предложение):** отказывать путям с префиксом `\\.\` / `\\?\pipe\`, или проверять `GetFileType == FILE_TYPE_DISK` на открытом дескрипторе, или ограничить чтение таймаутом. Нит ревью "брать `file.metadata()` открытого дескриптора" сам по себе это не лечит: для канала он тоже вернёт `is_file`.

Других дефектов, доказанных тестом, я не нашёл.

## 5. Verdict

**PASS** (SHIP). Все шесть критериев приёмки выполнены, fmt, clippy и 711 тестов workspace чистые, `files_e2e` и `update_e2e` проходят. Major и все minor-находки ревью исправлены, я сверил это по коду и тестам. Единственный найденный дефект (баг 1) срабатывает, только если Claude явно передаст путь именованного канала, и ломает лишь отправку файлов этим агентом до его перезапуска. Его стоит взять follow-up-ом вместе с уже записанными (уборка inbox, ответ ждущих `send_file` перед handover). Не проверено автоматически и остаётся на живую проверку после rollout: появление `mcp__cctg__send_file` после ⬆️ Обновить (`list_changed`), настоящие `getFile`/`sendPhoto` в Telegram.

Cleanup: копия `%TEMP%/qa032` удалена, сервисов и контейнеров не запускалось. Сборка в копии пересобрала артефакты `cctg` в общем target, поэтому процессному тесту, который запустят следом, может понадобиться `touch` крейта.

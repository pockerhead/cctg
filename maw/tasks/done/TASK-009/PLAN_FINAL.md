# PLAN_FINAL — TASK-009: hub — local transcript commands

Stage: plan-reviewer-2 (claude/opus, effort=medium). Пути даны относительно корня репозитория `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-009`. `REF` = `T/scratch/reviewer2/ws`. Это копия workspace (HEAD + 13 файлов), в которой реализован этот план. Она собрана и проверена: fmt, clippy `-D warnings`, `cargo test --workspace --offline` (140 passed, 1 ignored), 21 мутация (выжило 0), по 30 прогонов lib-тестов и `command_logs` на флейки (0 падений). Telegram не вызывался, `.env` не читался.

## 1. Summary

Hub получает команды `/brief [n] [session-id-prefix]` и `/full [n] [session-id-prefix]`. Callback поллинга только кладёт команду в неограниченную очередь. Один последовательный воркер резолвит транскрипт через узкий трейт `TranscriptLocator`: сейчас это `ProjectsDir`, который смотрит только `<projects_root>/<project>/<uuid>.jsonl`, в TASK-011 его заменит slot → current session. Дальше воркер в `spawn_blocking` читает не больше 256 MiB, прогоняет `transcript::parse` → `last_prompts` → `render_brief/render_full` и отправляет ровно вывод библиотеки. Короткий вывод уходит кусками `split_for_telegram` по порядку, длинный одним документом. На `400 … too long` недоставленный хвост один раз уходит документом. Смещение `getUpdates` хранится в `<CCTG_STATE_DIR|.cctg>/offset` (temp + fsync + rename) и сохраняется до передачи батча обработчикам (at most once). Неудачное сохранение повторяется дважды, потом логируется, и поллинг идёт дальше. Следующее смещение равно максимальному `update_id` батча + 1. Смещение старше 24 ч при старте игнорируется.

## 2. Implementation steps

### Step 0. Изоляция и baseline

1. `git status --short -- Cargo.toml Cargo.lock crates` должен быть пустым. Если нет, остановиться и не затирать чужие изменения.
2. Не открывать `.env`, не ходить в Telegram, не трогать `~/.claude`.
3. Cargo-команды запускать по одной (памяти на хосте мало). Target dir вне репозитория и **свой для каждой копии workspace**: Cargo может подхватить тестовые бинарники другой копии с теми же именами пакетов.
   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task009-impl-target'
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --offline -- -D warnings
   cargo test --workspace --offline
   ```
   Ожидание на HEAD: всё зелёное, 110 passed + 1 ignored.

### Step 1. Скопировать 13 файлов из REF байт в байт

Источник: `T/scratch/reviewer2/ws/<path>`, назначение `<path>` в корне репозитория. Файлы в REF в UTF-8 без BOM, с LF. Руками не править. Потом из корня репозитория:

```bash
sha256sum -c maw/tasks/in_progress/TASK-009/scratch/reviewer2/hashes.txt
```

Все 13 строк должны быть `OK`. Если хэш не сошёлся, скопировать файл заново. Полный diff против HEAD лежит в `T/scratch/reviewer2/final.diff`, отличия от reference планировщика в `T/scratch/reviewer2/vs_planner.diff`. Хэши планировщика (`scratch/planner/hashes.txt`) **не использовать**: 4 файла там устарели.

| Path | Вид | SHA-256 |
|---|---|---|
| `Cargo.lock` | edit (+`"transcript"` в deps `cctg`) | `01e4ba49a906a40ca9abe21f26e2619c5409f6f54724cc508861b33daf4a0f6e` |
| `crates/cctg/Cargo.toml` | edit (`transcript = { path = "../transcript" }`) | `6fa44c534620ee248751989ac6c025d0a5849f8cd76e7868c5437d90d4b2c00d` |
| `crates/cctg/src/hub/commands.rs` | new | `c990e1e4b4e958c8b9e4b4d6a2b5078817c5da08fa2e6276c970383b670cae23` |
| `crates/cctg/src/hub/config.rs` | edit | `374bdf6b49175397b82faf29a109535fc7b236f0e091e0b281c8f9786cc46754` |
| `crates/cctg/src/hub/mod.rs` | edit | `0ead6b3b6e5b9693e5869a1561b684b98e0385f82d94f21e40317246af1288d6` |
| `crates/cctg/src/hub/offset.rs` | new | `ad1f2eb45f46f762c30be2255bcbd5dab00b2ee9305e7ea9c28ab14d6374f1cb` |
| `crates/cctg/src/hub/sessions.rs` | new | `e7e546d24097347c01523934801bafe1576c6210c40c30c8d10732b8ea2f9c45` |
| `crates/cctg/src/hub/testdir.rs` | new (`#[cfg(test)]`) | `0de8e75a1f4d4c84871cd32860c2fdf9e5805e316fb6104225ba1793153b88c1` |
| `crates/cctg/src/hub/updates.rs` | edit | `66f081c7affcd638206cb87f671575d9079d506d6fc024e3f6dac7123db5ebe5` |
| `crates/cctg/tests/command_logs.rs` | new | `f0a90e1c9c0c818722d080305d2fdc75d582006c29e5bdb452f1228d3866bce1` |
| `crates/transcript/src/lib.rs` | edit (re-export `last_prompts`) | `72ae5623fa36b21c7d174f975baa78e2f89483cb6478e874cd5fadebfe19d788` |
| `crates/transcript/src/render.rs` | edit | `7b101b2868e2fed220b4e83b84b704a9b624ce4f7680dd15c59ea16c7f8fc31e` |
| `crates/transcript/tests/render.rs` | edit (+2 теста) | `6907398e4a57bf429953d89a37ae4c5d49578388b4458f353e4addedb6ed5cc5` |

Что в каждом файле и зачем:

**1a. `crates/transcript/src/render.rs`, `lib.rs`.** `pub fn last_prompts(turns: &[Turn], n: usize) -> &[Turn]` возвращает хвост, который начинается с n-го промпта с конца. Если промптов меньше n, возвращается весь slice, при `n == 0` пустой. Приватный `is_prompt` вынесен из `tool_after` без изменения поведения: meta `<channel>` и slash-команды считаются промптом, tool_result, service-префиксы и скрытый meta нет. Ни IO, ни новых зависимостей (`tests/purity.rs` это проверяет).

**1b. `crates/transcript/tests/render.rs`.** Тесты `last_prompts_keeps_the_last_n_exchanges` (n=1/2/99/0, пустой вход, `FINAL_ANSWER_BRIEF/FULL` без изменений) и `last_prompts_counts_only_what_renders_as_a_prompt`.

**1c. `crates/cctg/Cargo.toml`, `Cargo.lock`.** Единственное изменение зависимостей: локальный `transcript`. Внешних крейтов нет, сборка `--offline`.

**1d. `hub/config.rs`.** Добавлены `CCTG_PROJECTS_DIR` (по умолчанию `<USERPROFILE|HOME>/.claude/projects`, на Windows сначала `USERPROFILE`) и `CCTG_STATE_DIR` (по умолчанию `.cctg`, уже в `.gitignore`). Поля `projects_dir: Option<PathBuf>` и `state_dir: PathBuf`. `CLAUDE_CONFIG_DIR` не читается (решение оркестратора). Значения не логируются и не попадают в process env. Тест `paths_have_defaults_and_overrides`.

**1e. `hub/offset.rs`.** `OffsetStore::open(dir)` делает `create_dir_all`. `load()` работает так:
- файла нет → `None`;
- файл старше `MAX_AGE` = 24 ч по mtime → warning и `None`. Telegram хранит апдейт не больше 24 ч, так что такое смещение ничего не защищает. А после недели без апдейтов id начинаются со случайного значения, которое может оказаться ниже сохранённого;
- мусор в файле или ошибка чтения → warning только с `ErrorKind` и `None`.

`save(i64)` пишет `offset.tmp`, делает `writeln!`, `sync_all` и `rename`. Путь и значение не логируются. Тесты: round-trip между экземплярами, мусор и брошенный `.tmp`, `an_offset_older_than_a_day_is_ignored`.

**1f. `hub/updates.rs`.**
- `route_batch`: следующее смещение равно `max(update_id в батче) + 1`, **даже если это ниже текущего**. Если id в батче нет, остаётся текущее. Раньше было `max(old, new)`: после сброса id Telegram-ом такой апдейт приходил бы снова и обрабатывался каждую секунду (баг go-telegram-bot-api #156).
- Трейт `UpdateSource { chat_id(); get_updates(offset, timeout) }` с impl для `BotApi` (делегирование) и фейками в тестах.
- `poll<S: UpdateSource>(source, allowlist, store, handle)`: смещение загружается один раз при старте. Для батча с новым смещением сначала `save_offset(store, next).await`, потом `for_each(handle)`.
- `save_offset`: одна попытка плюс два повтора через 100 мс и 500 мс (на Windows антивирус или индексатор может ненадолго держать файл, и rename падает). Если всё равно не вышло, `warn!(kind)` и батч **всё равно** передаётся обработчикам. Поллинг никогда не блокируется на диске.
- Тесты: `saved_offset_prevents_handling_an_update_twice_after_restart`, `offset_is_saved_before_the_batch_is_handled`, `failing_offset_saves_do_not_stop_polling`, `a_briefly_failing_offset_save_is_retried`, `next_offset_follows_the_batch_even_below_the_old_one`, `ids_restarted_below_the_saved_offset_are_handled_once`.

**1g. `hub/sessions.rs`.** `Located { session_id, project, path }`, типизированный `LocateError { RootMissing, RootUnreadable(ErrorKind), NoSessions, NoMatch, Ambiguous(Vec<Located>) }`, трейт `TranscriptLocator: Send + Sync + 'static` с `locate(thread_id, session_prefix)` и `ProjectsDir::new(root)`. Сканирование идёт ровно на два уровня: каталоги в root, в них прямые файлы с именем `<lowercase-uuid>.jsonl`, `DirEntry::metadata().is_file()` (каталоги и symlink отбрасываются). Сортировка по mtime от новых к старым, при равенстве по id. Без префикса берётся первый. С префиксом: 0 совпадений → `NoMatch`, 1 → эта сессия, больше → `Ambiguous`. Префикс только сравнивается со stem и никогда не подставляется в путь. `thread_id` пока игнорируется, это шов для TASK-011.

**1h. `hub/commands.rs`.**
- `parse(text, bot_username)`: `/brief|/full` без учёта регистра, `@bot` без учёта регистра, чужой бот → `NotOurs`. Аргументы: `[] | [n] | [prefix] | [n prefix]`, n в 1..=100, по умолчанию brief 3 и full 1. Префикс из `[0-9a-fA-F-]` приводится к lowercase. Префикс из одних цифр требует явного n. Всё остальное → `Usage`.
- `prepare` → `prepare_limited(…, MAX_TRANSCRIPT_BYTES = 256 MiB)`:
  1. locate;
  2. `read_limited`: `File::open`, отказ по `metadata().len()`, потом `take(limit + 1)` и повторная проверка на случай, если файл вырос;
  3. `from_utf8_lossy` → `parse` → `last_prompts` → renderer.

  Notices без пути и без имени проекта: не найден (`NotFound`), не читается (`ErrorKind`), слишком большой, пусто, нет сессий, нет совпадения, неоднозначно (до 10 кандидатов `id · project` + «… и ещё N»), каталога проектов нет.
- `Reply { body, file_name, caption, short_id }`. **`body` содержит ровно вывод библиотеки, без заголовка.** `caption` = `"<view> · <short id> · последние N"` (без имени проекта), `file_name` = `"<view>-<short id>.txt"`.
- `deliver`: `split_for_telegram(body)`. Если `prefer_file`, уходит один документ с полным body. Иначе куски отправляются по очереди, каждый ждёт своей доставки. На первый `ApiError::Telegram{400, "…too long…"}` уходит один документ с `chunks[index..].concat()`, и доставка заканчивается. Текстом документ не повторяется, другие ошибки завершают ответ.
- `handle` не паникует и ошибок наружу не возвращает (паника `spawn_blocking` превращается в notice). `serve` обрабатывает по одной команде до закрытия канала.
- Логи: `info!(?view, prompts, session=<short id>)`, `warn!(session, kind)`, `warn!(session, "transcript too large to read")`, `warn!(?view, %error)`. `ApiError` уже очищен от URL с токеном (TASK-008).

**1i. `hub/mod.rs`.** Регистрирует модули `commands`, `offset`, `sessions` и `#[cfg(test)] testdir`. `run`:
1. `projects_dir` через `.with_context` (текст называет `CCTG_PROJECTS_DIR`, пути в нём нет);
2. `OffsetStore::open` (текст называет `CCTG_STATE_DIR`);
3. scheduler;
4. `unbounded_channel`;
5. `tokio::spawn(commands::serve(...))`;
6. `updates::poll(api, allowlist, &offsets, route_inbound(&commands_tx))`.

`route_inbound` вынесен в отдельную функцию, чтобы тест проверял именно production callback: команда уходит в `send`, прочий `Input` пишется в `info!(thread)`, `Callback` в `info!`, остальное игнорируется. Тест `a_slow_command_does_not_hold_up_polling` (multi_thread) держит первую команду в `locate` и проверяет, что оба батча выбраны, смещение сохранено как 3 и ничего не отправлено. После открытия gate оба ответа приходят по порядку и совпадают с библиотекой.

**1j. `hub/testdir.rs`.** Самоудаляющийся temp-каталог для unit-тестов, без крейта `tempfile`.

**1k. `tests/command_logs.rs`.** Отдельный test binary с `set_global_default` и `.without_time()`. Каталог проекта содержит маркер `private-marker-<pid>`. Проверяется, что в логах есть `transcript command answered`, `transcript cannot be read` и `5e551017`, но нет маркера, и что notices тоже без маркера.

### Step 2. Проверки

По одной команде, `CARGO_TARGET_DIR` как в Step 0:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
git status --short
```

Ожидание: fmt и clippy чистые, **140 passed, 1 ignored**. Из них cctg lib 58 (+1 ignored), main 1, `command_logs` 1, `routing_logs` 1, `stdout` 3; transcript 10+15+3+19+14+14 и 1 doc-test. В `git status` 13 файлов из таблицы (плюс артефакты задачи в `T/`), без `target/` и `.cctg/`.

Флейки: `cargo test -p cctg --offline --no-run`. Бинарники `Executable unittests src\lib.rs (...)` и `tests\command_logs.rs (...)` запустить по 30 раз с `-q`. Ожидается 0 падений (REF: `T/scratch/reviewer2/flake.out.txt`).

Утечки в diff: `git diff --cached | Select-String -Pattern 'Users[\\/-]user|AppData'` должен быть пуст. Допустимы синтетические `C--proj-*` и `C--Users-private-marker-<pid>-dev`. Токенов и user id в новых файлах нет (проверено grep по `final.diff`).

### Step 3. `T/IMPL_SUMMARY.md`

Туда записать: список файлов, результаты Step 2 и флейк-прогона, окно потерь at-most-once (см. Rollout). Живой `/brief` round-trip и RSS под long poll не блокируют merge (решение оркестратора): записать «не выполнено, ручной smoke после merge».

### Step 4. Коммит

Ветка `feature/hub-transcript-commands`, в коммит продукта идут 13 файлов. Не коммитить `.env`, `.claude/`, `.cctg/`, `target/`, scratch. Сообщение на английском без трейлеров "Generated with"/"Co-Authored-By", например: `hub: /brief and /full from local transcripts, persisted getUpdates offset`.

## 3. Test plan

| Критерий | Тест | Ожидание |
|---|---|---|
| 1. вывод совпадает с библиотекой, порядок кусков | `commands::tests::replies_match_the_library_on_fixtures` | 7 фикстур × (`/brief`, `/full 2`, `/brief@cctg_bot 100 5e55`): отправленные тексты подряд равны `split_for_telegram(render_*(last_prompts(parse(f), n))).chunks`, тот же `thread_id` |
| 1 | `multi_chunk_reply_keeps_order`, `transcript` `last_prompts_*` | 2-4 куска, конкатенация равна телу; срез по промптам совпадает с правилами рендера |
| 2. крупный вывод документом | `large_reply_goes_as_one_document` | больше 4 кусков: один `SendDocument`, байты равны телу, имя `full-5e551017.txt`, caption `full · 5e551017 · последние 8` |
| 2. 400 too long → документ один раз | `too_long_switches_to_a_document_once` | отказ на 2-м куске даёт [Send, Send, SendDocument(chunks[1..])], причём `chunks[0] + документ == тело`. Если отвергается всё: ровно [Send, SendDocument], дальше ничего |
| 2 | `other_errors_do_not_switch_to_a_document` | 400 thread not found: одна попытка, без документа |
| 3. без повтора после рестарта | `saved_offset_prevents_handling_an_update_twice_after_restart`, `offset_is_saved_before_the_batch_is_handled`, `offset::tests::*` | повторно обрабатывается только новый апдейт; на момент вызова handler смещение уже на диске; round-trip, мусор, `.tmp`, смещение старше 24 ч игнорируется |
| 3 | `ids_restarted_below_the_saved_offset_are_handled_once`, `next_offset_follows_the_batch_even_below_the_old_one` | апдейт с id ниже сохранённого смещения обрабатывается один раз, на диске 8 |
| 4. плохой путь → понятный notice, поллинг жив | `bad_paths_get_a_notice_and_the_worker_keeps_going`, `missing_and_unreadable_files_become_notices`, `oversized_transcripts_become_a_notice` | каждый случай получает свой notice без пути, следующая команда выполняется |
| 4 | `a_slow_command_does_not_hold_up_polling`, `failing_offset_saves_do_not_stop_polling`, `a_briefly_failing_offset_save_is_retried` | зависшая команда и сломанный диск не останавливают поллинг; короткий сбой сохранения лечится повтором |
| 5. нет токена, user id, приватных путей | `tests/command_logs.rs`, существующие `routing_logs`, token/fixture privacy tests, grep diff | маркера пути нет ни в логах, ни в notices |
| 6. резолвер | `sessions::tests::*`, `ambiguous_prefix_lists_candidates` | самая новая по mtime сессия среди всех проектов, уникальный/полный/неоднозначный префикс, отсутствующий root, мусор пропускается |
| 7. субагенты не сессии | `subagent_transcripts_are_never_sessions` | более свежие `agent-*` и файлы с UUID-именем в `<uuid>/subagents/` не выбираются |
| 8. существующие тесты | Step 2 | все прежние тесты зелёные |

Мутации (`T/scratch/reviewer2/mutate.py` → `mutations.out.txt`): убиты все 21. Это 12 мутаций планировщика плюс: заголовок в ответе; документ дублирует принятый кусок; лимит размера не проверяется; сохранение после обработки; нет повтора сохранения; правило PLAN_V2 «не обрабатывать без сохранения»; смещение не опускается ниже старого; старое смещение загружается; callback поллинга ждёт команду.

## 4. Rollout notes

- **Миграций нет.** Новый файл состояния `<CCTG_STATE_DIR|.cctg>/offset` относительно cwd hub, `.cctg/` уже в `.gitignore`. При первом запуске файла нет, hub обработает висящие неподтверждённые апдейты (не старше 24 ч). Это безвредно.
- **Env:** `CCTG_PROJECTS_DIR` и `CCTG_STATE_DIR` опциональны. Если нет ни override, ни home, `run` падает на старте с текстом, который называет `CCTG_PROJECTS_DIR`. При нестандартном `CLAUDE_CONFIG_DIR` пользователь прописывает `CCTG_PROJECTS_DIR` в `.env`. Feature flags нет.
- **At most once.** Краш после сохранения и до ответа теряет команды батча (включая уже поставленные в очередь воркера). Пользователь повторяет команду. Если сохранение не удалось после двух повторов, рестарт до следующего успешного сохранения может повторить этот батч. Это сознательный обмен ради живого поллинга.
- **Windows rename:** std не обещает атомарность. Битый файл даёт warning и старт без смещения.
- **Размер транскрипта:** лимит чтения 256 MiB. Самая большая реальная сессия на этой машине 92 MiB, её разбор даёт пик рабочего набора от 112 MB за 162 мс (`T/scratch/reviewer2/probe.out.txt`). Хвостовое чтение появится в TASK-016.
- **Резолвер до TASK-011** берёт глобально самую свежую по mtime сессию, обычно ту, что сейчас активна. Какая сессия выбрана, видно только в caption документа и в логе (short id), в текстовом ответе этого нет. Явный префикс снимает неоднозначность.
- **Имена проектов** (`C--Users-<name>-…`) видны пользователю в Telegram только в списке неоднозначных кандидатов и никогда не логируются.
- **Очередь команд без ограничения:** писать могут только пользователи из allowlist, отправку ограничивает bucket планировщика.
- **Остаётся из TASK-008:** если hub работает без перерыва неделю без апдейтов и Telegram отдаёт id ниже смещения, новое правило `route_batch` это лечит. Если же сервер такие апдейты вообще не отдаёт, это проверить без живого Telegram нельзя. Рестарт hub в этом случае помогает (смещение старше 24 ч игнорируется).
- **Живой smoke** после merge (решение оркестратора): `cctg hub` с реальным `.env`, `/brief`, `/full 1 <prefix>`, рестарт, команда не повторилась.

## 5. Review notes

Контрпример (disconfirmation), проверен первым: сохранённое на диск смещение делает hub глухим или зацикленным после долгой паузы. Bot API прямо пишет: «If there are no new updates for at least a week, then identifier of the next update will be chosen randomly instead of sequentially», и «updates … will not be kept longer than 24 hours» ([Update](https://core.telegram.org/bots/api#update), [getUpdates](https://core.telegram.org/bots/api#getupdates)). В go-telegram-bot-api #156 бот после такого сброса бесконечно получал один и тот же апдейт ([issue](https://github.com/go-telegram-bot-api/telegram-bot-api/issues/156)). **Контрпример подтвердился:** в reference `route_batch` берёт `max(old, new)`, поэтому апдейт с id ниже сохранённого смещения обрабатывался бы примерно раз в секунду бесконечно (мутационный тест `ids_restarted_below_…` это показывает). Исправлено в двух местах: следующее смещение считается от батча, смещение старше 24 ч игнорируется.

Отличия от PLAN_V2:
1. **Отклонено «не передавать батч, пока смещение не сохранено, повторять бесконечно».** Если каталог состояния станет недоступен для записи, hub целиком остановится: не будет ни команд, ни будущих permission-кнопок. Это ровно тот путь, который «останавливает poll loop». Вместо этого: два повтора (они покрывают кратковременную блокировку файла на Windows), потом лог и обработка батча. Тесты: `failing_offset_saves_do_not_stop_polling`, `a_briefly_failing_offset_save_is_retried`. Порядок «сначала сохранить, потом обработать» закреплён тестом `offset_is_saved_before_the_batch_is_handled` (в PLAN_V2 теста на порядок не было).
2. **Лимит 64 MiB из PLAN_V2 заменён на 256 MiB.** Реальная top-level сессия на этой машине весит 96 315 756 байт, при 64 MiB `/brief` на ней выдал бы отказ. Размер замерен probe-скриптом, содержимое не читалось в логи. Тест лимита использует маленький лимит через `prepare_limited`, без многомегабайтных sparse-файлов.
3. **Принято из PLAN_V2:** заголовок убран, тело ответа ровно равно выводу библиотеки. Caption без имени проекта. Тест fallback проверяет `принятый кусок + документ == тело`. Добавлен тест независимости поллинга через production `route_inbound`.
4. **Уточнён инвариант fallback:** документ равен `chunks[index..].concat()`. Он совпадает с остатком тела, пока `split_for_telegram` не выбросил кусок из одних пробелов (для этого нужна пробельная серия около 4096 символов, практически не встречается).
5. **Ссылка на reference заменена:** `scratch/planner/ws` и его `hashes.txt` устарели в 4 файлах (`commands.rs`, `mod.rs`, `offset.rs`, `updates.rs`). Исполнитель копирует из `scratch/reviewer2/ws` по новым хэшам.
6. Проверено и не менялось: резолвер (symlink и субагенты), `last_prompts`, конфиг, отсутствие пути, токена и user id в логах и сообщениях об ошибках (`ApiError` без URL, `io::Error` без пути, notices без пути). Решения записаны в `T/log.jsonl`, предложение risk lesson добавлено в `T/PCTX_PROPOSALS.md`.

# PLAN — TASK-009: hub — local transcript commands

Stage: planner (claude/opus, effort=medium). Пути относительно корня репозитория `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-009`. `REF` = `T/scratch/planner/ws`: копия workspace с полной реализацией этого плана. REF собран и прогнан: `cargo fmt --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (132 passed, 1 ignored), 12 мутаций (все убиты), 60 прогонов на флейки (0 падений). Реальный Telegram не вызывался, `.env` не читался. Из `~/.claude/projects` смотрел только имена, количество и mtime файлов.

## 1. Understanding

### Что есть сейчас

**`crates/cctg/src/hub/` (TASK-008).**
- `mod.rs:45-83` `run`: `Config::load` → `BotApi::new` → `getMe` → `getChatMember` → `check_topic_rights` → spawn `Scheduler` → `updates::poll`. Колбэк poll только логирует `Routed::Input` (`mod.rs:69-77`), `outbox` держится живым "для TASK-009/011".
- `updates.rs:28-34` `Inbound { message_id, thread_id, text }`, без user id. `route_batch` (`:142-167`) считает следующий offset по любому целому `update_id`. `poll` (`:179-207`) принимает конкретный `&BotApi`, offset живёт только в памяти (`let mut offset = None`, `:180`). После рестарта hub первый `getUpdates` без offset снова получит всё неподтверждённое.
- `scheduler.rs`: `Outbox::submit(Op) -> oneshot::Receiver<Delivery>` (`:246-252`), трейт `Transport` (`:107-109`) для фейка в тестах, `Op::Send`/`Op::SendDocument` (`:37-48`) в одной FIFO-полосе сообщений, bucket 5 + 1 токен/4 с + 1 с зазор.
- `api.rs`: `Document { file_name, bytes, caption }` (`:134-139`), `ApiError::Telegram { code, description }` (`:32-33`); 400 с описанием приходит как `Telegram { code: 400, .. }` (`parse_envelope`, `:372-377`).
- `config.rs:71-146`: `Config { token, chat_id, allowlist }`, `from_vars(closure)` для тестов, значения никогда не эхоятся.
- `cctg` пока не зависит от крейта `transcript` (`crates/cctg/Cargo.toml`).

**`crates/transcript/src/` (TASK-005/006/007/020).**
- `lib.rs:131-135` `parse(&str) -> Vec<Turn>`; `render.rs:44-52` `render_brief`/`render_full(&[Turn])`; `split.rs:40-54` `split_for_telegram(text, SplitOptions{max_chunks: 4})` → `SplitResult { chunks, prefer_file }`, чанки это последовательные срезы текста (whitespace-only выброшены).
- Что считается промптом, знает только `render.rs`: приватные `user_text` (`:172-190`) и цикл в `tool_after` (`:242-261`). Слайсить "последние n промптов" снаружи значит дублировать эти правила (meta `<channel>`, slash-команды, service-префиксы, tool_result).
- `tests/purity.rs` запрещает IO и `#[cfg(test)]` в `src/`, фиксирует ровно 3 зависимости.

### Проверенная раскладка `~/.claude/projects` (только имена/mtime)

- 16 каталогов проектов, 76 файлов `*.jsonl` глубиной 1 внутри проекта, все с именем-UUID (`<8>-<4>-<4>-<4>-<12>.jsonl`, lowercase hex).
- 557 `*/subagents/agent-<id>.jsonl` глубиной 3 (`<project>/<session-uuid>/subagents/...`) плюс `.meta.json`.
- Кроме jsonl в проектах лежат `sessions-index.json`, `bridge-pointer.json`, каталоги `memory/`, `<uuid>/tool-results/*.txt`.
- Текущая сессия это самый свежий по mtime файл (8 MB); остальные 0.35-0.5 MB.

Вывод: "top-level сессия" = прямой файл-ребёнок каталога проекта с именем-UUID и расширением `.jsonl`. Рекурсия не нужна и вредна: субагенты лежат глубже и не видны по построению.

### Что даёт Telegram (research)

- `getUpdates.offset`: "An update is considered confirmed as soon as getUpdates is called with an offset higher than its update_id"; неподтверждённые хранятся до 24 ч ([Bot API getUpdates](https://core.telegram.org/bots/api#getupdates), [aiogram mirror](https://docs.aiogram.dev/en/latest/api/methods/get_updates.html)). Значит обработанный батч считается подтверждённым только на следующем вызове, и рестарт между ними повторяет батч. Лечится сохранением offset на диск.
- Команда в группе может прийти как `/start@jobs_bot` (entity `bot_command`, там же). Парсер снимает `@username` и игнорирует чужого бота.
- Лимит текста 4096 и `400 Bad Request: message is too long` проверены в `CLAUDE.md`.
- Атомарная запись: temp-файл в том же каталоге → `fsync` → `rename` ([0xkiire](https://0xkiire.com/crash-consistency-fsync-rename/), [LWN](https://lwn.net/Articles/789600/)). `std::fs::rename` на Windows это `MoveFileExW` с заменой, на Win10 1607+ с POSIX-семантикой через `FileRenameInfoEx` ([std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html)); сам std атомарность не обещает, `MoveFileEx` может молча откатиться в copy ([antonymale](https://antonymale.co.uk/windows-atomic-file-writes.html), [rust-atomicwrites#27](https://github.com/untitaker/rust-atomicwrites/issues/27)). Для 1 числа в том же каталоге это приемлемо: мусор в файле читается как "offset нет" с warning, hub стартует.
- `CLAUDE_CONFIG_DIR` переносит `projects/` ([issue #28808](https://github.com/anthropics/claude-code/issues/28808), [ccusage](https://ccusage.com/guide/claude/)); hub его не читает, покрывается `CCTG_PROJECTS_DIR` (см. Open questions).

## 2. Approach

1. **Резолвер за узким трейтом** (`hub/sessions.rs`): `TranscriptLocator::locate(thread_id, session_prefix) -> Result<Located, LocateError>`. Реализация `ProjectsDir` сканирует `<root>/<project>/<uuid>.jsonl`, сортирует по mtime (новые первыми, при равенстве по id), без префикса берёт первый, с префиксом: 0 → `NoMatch`, 1 → он, больше → `Ambiguous(все, новые первыми)`. `thread_id` сейчас игнорируется, но уже в сигнатуре: TASK-011 заменит реализацию на "слот этой темы → текущая сессия", не трогая вызовы. Это и есть условие flip из Resolved questions.
2. **`[n]` = последние n промптов**, новой чистой функцией `transcript::last_prompts(turns, n) -> &[Turn]` в `render.rs`. Она переиспользует тот же предикат промпта, что `tool_after` (вынесен в `is_prompt`). Тогда вывод hub буквально равен `render_*(last_prompts(parse(jsonl), n))`, что и требует критерий 1. Дефолты: brief 3, full 1; допустимо 1..=100.
3. **Команды** (`hub/commands.rs`): `parse` → `prepare` (locate + `std::fs::read` + `from_utf8_lossy` + parse + slice + render, в `spawn_blocking`) → `deliver`. Ответ = строка-заголовок `brief · <project> · <short-id> · последние N`, пустая строка, затем вывод библиотеки. Заголовок нужен, чтобы пользователь видел, какую сессию взяли "по mtime". Доставка: `split_for_telegram`; `prefer_file` → один `sendDocument` (`brief-<short>.txt`, caption = заголовок); иначе чанки по порядку, каждый ждёт свою доставку. Если Telegram отвечает 400 "too long" на чанк k, то чанки k..end уходят одним документом, и на этом всё: документ не ретраится текстом, повторного переключения нет. Остальные ошибки прекращают ответ и логируются.
4. **Один последовательный воркер** (`commands::serve`) с `mpsc::unbounded_channel` из колбэка poll. Колбэк синхронный и не должен ждать сеть; последовательная обработка гарантирует, что чанки двух ответов в одной теме не перемешаются. Отвечаем в тот же `thread_id`, откуда пришла команда (`None` = General).
5. **Offset на диске** (`hub/offset.rs`): `<state_dir>/offset`, десятичное число; запись через `offset.tmp` + `sync_all` + `rename`. `poll` становится generic по `UpdateSource` (трейт на `BotApi` плюс фейк в тестах), загружает offset при старте и **сохраняет новый offset до передачи батча обработчикам**. Выбор at-most-once: после краша между save и обработкой команда потеряется, но не ответит дважды. Ошибка save только логируется, поллинг не останавливается.
6. **Конфиг**: `CCTG_PROJECTS_DIR` (по умолчанию `<USERPROFILE|HOME>/.claude/projects`, на Windows сначала `USERPROFILE`), `CCTG_STATE_DIR` (по умолчанию `.cctg`, уже в `.gitignore`). `projects_dir: Option<PathBuf>`: `None` только без override и без home; тогда `run` падает на старте с понятным текстом. Существующие тесты конфига не меняются.
7. **Приватность**: логи несут только view, n и короткий id сессии. Путь и имя каталога проекта (`C--Users-<name>-...` это тот же приватный путь) никогда не логируются. `io::Error` в сообщения пользователю не попадает целиком, только `ErrorKind`. Отдельный test binary с глобальным subscriber это проверяет маркером в имени каталога.

Отвергнуто: резать отрендеренный текст по строкам `> ` (в full service-строки тоже `> `); `tokio::spawn` на каждую команду (перемешивание чанков); сохранять offset после обработки (дубли ответов после краша); рекурсивный обход проектов с фильтром `subagents` (хрупко, лишний IO); свой HTTP-фейк Telegram для теста рестарта (трейт `UpdateSource` проще и честнее моделирует offset). Решения записаны в `T/log.jsonl`.

## 3. Steps

### Step 0. Изоляция и baseline

PowerShell или Git Bash; cargo-команды по одной (памяти на хосте мало). Target dir вне репозитория:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task009-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание на HEAD: всё зелёное, **110 passed + 1 ignored** (cctg lib 31 + 1 ignored, main 1, routing_logs 1, stdout 3; transcript 10+15+3+17+14+14 и 1 doc-test). `git status --short -- Cargo.toml Cargo.lock crates` пусто, иначе остановиться и не затирать чужое. `.env` не открывать, в Telegram не ходить, `~/.claude` не трогать (тесты его не читают).

### Step 1. Скопировать 13 файлов из REF байт в байт

`REF = maw/tasks/in_progress/TASK-009/scratch/planner/ws`. Файлы в REF в UTF-8 без BOM, LF (репо с `core.autocrlf=true` нормализует при коммите). После копирования из корня репозитория:

```bash
sha256sum -c maw/tasks/in_progress/TASK-009/scratch/planner/hashes.txt
```

Все 13 строк `OK`. Если хэш не сошёлся, скопировать заново, руками не править. Полный diff против HEAD: `T/scratch/planner/proto.diff`.

| Path | Вид | SHA-256 |
|---|---|---|
| `Cargo.lock` | edit (+`"transcript"` в deps `cctg`) | `01e4ba49a906a40ca9abe21f26e2619c5409f6f54724cc508861b33daf4a0f6e` |
| `crates/cctg/Cargo.toml` | edit | `6fa44c534620ee248751989ac6c025d0a5849f8cd76e7868c5437d90d4b2c00d` |
| `crates/cctg/src/hub/commands.rs` | new | `ad18e700566da805184c19b305f35ee456be9c15b3eaa2a383f8c73cfea405b8` |
| `crates/cctg/src/hub/config.rs` | edit | `374bdf6b49175397b82faf29a109535fc7b236f0e091e0b281c8f9786cc46754` |
| `crates/cctg/src/hub/mod.rs` | edit | `35b49c78a26a82be2fc536f1bcec277b17544fd490b0a515c83ea64e5356b6be` |
| `crates/cctg/src/hub/offset.rs` | new | `e1a8aa21616a656100b352309d5b25421fa35b83a3c1976f0a6a2f4268647464` |
| `crates/cctg/src/hub/sessions.rs` | new | `e7e546d24097347c01523934801bafe1576c6210c40c30c8d10732b8ea2f9c45` |
| `crates/cctg/src/hub/testdir.rs` | new (`#[cfg(test)]`) | `0de8e75a1f4d4c84871cd32860c2fdf9e5805e316fb6104225ba1793153b88c1` |
| `crates/cctg/src/hub/updates.rs` | edit | `de6b9edd529174eced96e0a3dc655150228969baf87fe89e8a636e194a1b18b2` |
| `crates/cctg/tests/command_logs.rs` | new | `f0a90e1c9c0c818722d080305d2fdc75d582006c29e5bdb452f1228d3866bce1` |
| `crates/transcript/src/lib.rs` | edit (re-export) | `72ae5623fa36b21c7d174f975baa78e2f89483cb6478e874cd5fadebfe19d788` |
| `crates/transcript/src/render.rs` | edit | `7b101b2868e2fed220b4e83b84b704a9b624ce4f7680dd15c59ea16c7f8fc31e` |
| `crates/transcript/tests/render.rs` | edit (+2 теста) | `6907398e4a57bf429953d89a37ae4c5d49578388b4458f353e4addedb6ed5cc5` |

Что в каждом файле и зачем:

**1a. `crates/transcript/src/render.rs`, `lib.rs`.** Новая `pub fn last_prompts(turns: &[Turn], n: usize) -> &[Turn]`: хвост, начинающийся с n-го промпта с конца; все turns, если промптов меньше; пусто при `n == 0`. Предикат `is_prompt(turn)` вынесен из `tool_after` без изменения поведения (user-turn, у которого хотя бы один `Text` классифицируется `user_text` как `Prompt`). `lib.rs` добавляет `last_prompts` в `pub use render::{...}`. Purity соблюдена: ни IO, ни новых зависимостей.

**1b. `crates/transcript/tests/render.rs`.** `last_prompts_keeps_the_last_n_exchanges` (фикстура `final_answer`: n=1 даёт второй обмен, n=2 и n=99 совпадают с существующими `FINAL_ANSWER_BRIEF`/`FULL`, n=0 и пустой вход пусты); `last_prompts_counts_only_what_renders_as_a_prompt` (tool_result, `<task-notification>`, скрытый meta не граница; meta `<channel>` граница).

**1c. `crates/cctg/Cargo.toml`, `Cargo.lock`.** `transcript = { path = "../transcript" }` в `[dependencies]`. Новых внешних крейтов нет, сборка `--offline`.

**1d. `hub/config.rs`.** Константы `PROJECTS_VAR = "CCTG_PROJECTS_DIR"`, `STATE_VAR = "CCTG_STATE_DIR"`, `DEFAULT_STATE_DIR = ".cctg"`. Поля `projects_dir: Option<PathBuf>`, `state_dir: PathBuf`. Замыкание `required` теперь поверх нового `optional` (trim, пустое = нет). Home: на Windows `USERPROFILE`, затем `HOME`; иначе `HOME`. Тест `paths_have_defaults_and_overrides`.

**1e. `hub/offset.rs`.** `OffsetStore::open(dir)` (create_dir_all), `load() -> Option<i64>` (нет файла → `None`; мусор или ошибка чтения → warning только с `ErrorKind` и `None`), `save(i64)` (`offset.tmp` → `writeln!` → `sync_all` → `rename`). Пути не логируются.

**1f. `hub/updates.rs`.** Трейт `UpdateSource { chat_id(); get_updates(offset, timeout) -> impl Future<...> + Send }` и impl для `BotApi` (делегирует в inherent-методы). `poll<S: UpdateSource>(source, allowlist, store: &OffsetStore, handle)`: `offset = store.load()`; после `route_batch`, если offset изменился, `store.save(next)` до `for_each(handle)`; ошибка save это `warn!(kind = ?..)` и дальше. Тест `saved_offset_prevents_handling_an_update_twice_after_restart` с фейком, который, как Telegram, отдаёт все апдейты `>= offset` и никогда их не забывает (см. Test plan).

**1g. `hub/sessions.rs`.** `Located { session_id, project, path }`, `LocateError { RootMissing, RootUnreadable(ErrorKind), NoSessions, NoMatch, Ambiguous(Vec<Located>) }`, трейт `TranscriptLocator: Send + Sync + 'static`, `ProjectsDir::new(root)`. Сканирование: `read_dir(root)` → только каталоги → `read_dir(project)` → имя `<uuid>.jsonl` (`is_session_id`: 36 символов, `-` на 8/13/18/23, остальное `[0-9a-f]`) → `metadata().is_file()` (каталоги и symlink не проходят) → `modified()` (ошибка = `UNIX_EPOCH`). Нечитаемый каталог проекта пропускается, не валит команду.

**1h. `hub/commands.rs`.**
- `parse(text, bot_username) -> Parsed::{Command, Usage, NotOurs}`. `/brief`/`/full` без учёта регистра, `@bot` чужого бота → `NotOurs`. Аргументы: `[]`; `[digits]` → n; `[другое]` → префикс; `[n, prefix]`; иначе `Usage`. Префикс: только `[0-9a-fA-F-]`, приводится к lowercase (в пути не подставляется, только сравнение со stem). n вне 1..=100 → `Usage`. Префикс из одних цифр требует явного n (`/brief 1 0133`).
- `prepare(locator, thread_id, command) -> Prepared::{Transcript(Reply), Notice(String)}` в `spawn_blocking`. Нет файла (`NotFound`, нормальное состояние по домену transcript) → notice "не найден"; другая ошибка чтения → warn (`session`, `kind`) + notice с `ErrorKind`; пустой рендер → "пока нечего показывать". Тексты notice без путей.
- `deliver(outbox, thread_id, reply)`: см. Approach п.3; `is_too_long` = `ApiError::Telegram { code: 400, description }` c `"too long"` в lowercase-описании.
- `handle(input, outbox, locator, bot_username)` никогда не паникует и не возвращает ошибку наружу; `serve(rx, outbox, locator, bot_username)` обрабатывает по одной до закрытия канала.
- Лог: `info!(?view, prompts, session = <short id>, "transcript command answered")`, `warn!(?view, %error, "transcript command reply failed")`. `ApiError` Display без токена (TASK-008 `without_url`).

**1i. `hub/mod.rs`.** `pub mod commands; pub mod offset; pub mod sessions; #[cfg(test)] pub(crate) mod testdir;`. В `run` после `Config::load`: `projects_dir` через `.with_context` (текст называет `CCTG_PROJECTS_DIR`, не путь), `OffsetStore::open(&config.state_dir)` (текст называет `CCTG_STATE_DIR`). После spawn scheduler: `unbounded_channel`, `tokio::spawn(commands::serve(rx, outbox, Arc::new(ProjectsDir::new(projects_dir)), me.username.clone()))`. Колбэк poll: `Routed::Input(input) if commands::is_command(&input)` → `tx.send(input)` (ошибка → warn), прочие ветки как были. Старый комментарий "Handlers arrive with TASK-009/011" удалён.

**1j. `hub/testdir.rs`.** Самоудаляющийся `TempDir` для unit-тестов (`std::env::temp_dir()` + pid + счётчик). Внешний `tempfile` не нужен.

**1k. `tests/command_logs.rs`.** Отдельный binary, `set_global_default` (работа в `spawn_blocking` идёт на другом потоке, scoped subscriber её не видит; урок TASK-008 про callsite registration), `.without_time()`. Каталог проекта `C--Users-private-marker-<pid>-dev` под `CARGO_TARGET_TMPDIR`: успешные `/brief`, `/full 1 5e55`, отсутствующий файл, каталог вместо файла. Проверка: в логах есть `transcript command answered`, `transcript cannot be read` и `5e551017`, но нет маркера; notice-сообщения тоже без маркера.

### Step 2. Проверки

По одной команде, `CARGO_TARGET_DIR` как в Step 0:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
git status --short
```

Ожидание (прогон REF): fmt/clippy чистые; **132 passed, 1 ignored**: cctg lib 50 (+1 ignored), main 1, `command_logs` 1, `routing_logs` 1, `stdout` 3; transcript 10+15+3+19+14+14, doc 1. `git status`: 13 файлов из таблицы плюс артефакты задачи; `target/` и `.cctg/` в репо не появляются.

Флейки: `cargo test -p cctg --offline --no-run`, взять `Executable unittests src\lib.rs (...)` и `tests\command_logs.rs (...)`, каждый 30 раз с `-q`; ожидание 0 падений (REF: `T/scratch/planner/flake.out.txt`, 0 из 60).

Проверка утечек в diff: `git diff --cached | Select-String -Pattern 'Users[\\/-]user|AppData'` пусто (синтетические `C--proj-*`, `C--Users-private-marker-<pid>-dev` допустимы). Токенов и user id в новых файлах нет по построению (команды их не касаются).

### Step 3. `T/IMPL_SUMMARY.md`

Список файлов, результаты Step 2, результат прогона на флейки. Живой замер RSS под long poll (перенесён из TASK-008) исполнитель в песочнице сделать не может: записать "не выполнено, нужен ручной smoke" (см. Open questions).

### Step 4. Коммит

Ветка `feature/hub-transcript-commands`. В коммит продукта 13 файлов. Не коммитить `.env`, `.claude/`, `.cctg/`, `target/`, scratch. Сообщение на английском без "Generated with"/"Co-Authored-By", например `hub: /brief and /full from local transcripts, persisted getUpdates offset`.

## 4. Test plan (критерии → доказательства)

| Критерий | Тест | Что доказывает |
|---|---|---|
| 1. `/brief`/`/full` = вывод библиотеки, порядок кусков | `commands::tests::replies_match_the_library_on_fixtures` | 7 фикстур transcript × (`/brief`, `/full 2`, `/brief@cctg_bot 100 5e55`): отправленные тексты ровно `split_for_telegram(header + "\n\n" + render_*(last_prompts(parse(f), n))).chunks` подряд, в тот же `thread_id` |
| 1 | `multi_chunk_reply_keeps_order` | синтетические 3 обмена по ~2.7k: 2-4 чанка, порядок совпадает, конкатенация = полный текст |
| 1 | `transcript` `last_prompts_*` | слайс по промптам совпадает с правилами рендера |
| 2. крупный вывод документом | `large_reply_goes_as_one_document` | >4 чанков: ровно один `SendDocument`, байты = полный текст, имя `full-5e551017.txt`, caption = заголовок |
| 2. 400 "too long" один раз → документ | `too_long_switches_to_a_document_once` | отказ на 2-м чанке: [Send, Send, SendDocument(чанки[1..])]; Telegram отвергает всё: ровно [Send, SendDocument], дальше ничего |
| 2 | `other_errors_do_not_switch_to_a_document` | 400 "thread not found": одна попытка, без документа |
| 3. offset переживает рестарт | `updates::tests::saved_offset_prevents_handling_an_update_twice_after_restart` | первый прогон обработал update 5; новый `OffsetStore` над тем же каталогом грузит 6; фейк всё ещё держит 5, обработан только 6; контроль без файла обрабатывает оба |
| 3 | `offset::tests::*` | round-trip между экземплярами, нет `.tmp` после save, мусорный файл и брошенный `.tmp` не ломают старт |
| 4. нечитаемый/отсутствующий путь → понятное сообщение, поллинг жив | `bad_paths_get_a_notice_and_the_worker_keeps_going` | в одном воркере подряд: нет совпадения, успех, usage; потом нет сессий ×2; пустой рендер; нет каталога проектов. Каждое получает свой notice, воркер идёт дальше |
| 4 | `missing_and_unreadable_files_become_notices` | `NotFound` и чтение каталога дают разные notice без пути |
| 4 | (конструктивно) | воркер это отдельная задача; колбэк poll только `send` в канал, `handle` не возвращает ошибок |
| 5. нет токена, user id, приватных путей | `tests/command_logs.rs::command_logs_carry_no_paths` | TRACE-логи всех веток без маркера из имени проекта/пути; notice без маркера. Плюс grep diff из Step 2. Фикстуры не добавляются |
| 6. резолвер: последний по mtime, префикс, кандидаты, узкий интерфейс | `sessions::tests::newest_session_wins_without_a_prefix`, `prefix_selects_one_or_lists_candidates`, `missing_root_is_reported`, `non_session_entries_are_skipped`, `session_id_shape`; `commands::tests::ambiguous_prefix_lists_candidates` | новейший по mtime через проекты; уникальный и полный префикс; неоднозначный → список, новые первыми, не больше 10 + "и ещё N"; чужие имена, каталоги `.jsonl`, файлы в корне пропущены |
| 7. `subagents/*.jsonl` никогда не top-level | `subagent_transcripts_are_never_sessions` | более свежие `agent-*.jsonl` и файл с именем-UUID внутри `<uuid>/subagents/` не выбираются ни по mtime, ни по префиксу |
| 8. существующие тесты | Step 2 | все прежние тесты зелёные, их код не менялся (кроме нового теста в `config.rs`, `updates.rs`, `tests/render.rs`) |

Мутации (REF, `T/scratch/planner/mutations.out.txt`, скрипт `mutate.py`), все 12 убиты: offset не сохраняется; offset не грузится; "too long" не распознаётся; документ ретраится текстом; `prefer_file` игнорируется; каталоги принимаются за сессии; любое имя файла принимается; старейший вместо новейшего; неоднозначный префикс угадывается; путь в warn-логе; весь транскрипт вместо последних n; `last_prompts` off-by-one.

## 5. Risk areas

- **At-most-once.** Краш между `save` и обработкой теряет команды этого батча без ответа. Выбрано сознательно (дубль ответа хуже); пользователь просто повторит команду.
- **Атомарность rename на Windows** не гарантирована std; при сбое возможен пустой/битый `offset`. Последствие ограничено: warning и старт без offset, то есть повтор последнего батча, не падение.
- **Запись offset на каждый непустой батч**: один маленький fsync на батч. При нашем трафике (люди в одной группе) это ничто; если станет заметно, писать не чаще раза в N секунд.
- **Большие транскрипты.** Текущая сессия 8 MB, файл читается целиком и парсится в blocking-пуле при каждой команде. Для сотен MB это память и секунды. Лимита нет; рендер ограничен `n` промптов, но parse идёт по всему файлу. Если станет проблемой, читать хвост (TASK-016 всё равно заводит tail по смещению).
- **Файл пишется параллельно** Claude Code: последняя строка может быть обрезана. `parse` пропускает битые строки, `from_utf8_lossy` переживает обрезанный UTF-8. Открытие на Windows: std открывает с `FILE_SHARE_READ|WRITE|DELETE`, чтение не блокирует писателя (если писатель сам не запретил share-read; на практике Claude Code не запрещает, иначе `/brief` на живой сессии даст notice с `PermissionDenied`, не падение).
- **"Самая свежая по mtime"** почти всегда текущая активная сессия, в том числе сессия maw runner или эта. Это ожидаемое поведение до TASK-011; заголовок ответа показывает, какую сессию взяли.
- **Имя каталога проекта уходит в Telegram** (в заголовке и списке кандидатов). Это закрытая группа allowlisted-пользователя; в логи оно не попадает (тест). Если пользователь против, заменить на хвост имени.
- **Неограниченный канал команд.** Писать могут только allowlisted-пользователи; флуд своими командами упрётся в bucket планировщика, память растёт медленно. Приемлемо.
- **Документ >50 MB** (лимит загрузки ботом без local server) даст ошибку Telegram; она логируется, пользователь ответа не получит. При `n <= 100` и обрезке full (500/1500 символов на вход/результат) нереально.
- **Первый живой запуск** обработает висящие старые апдейты бота, если там есть `/brief`; безвредно.
- **Scheduler starvation** (IMPL_REVIEW TASK-008) не задет: команды шлют только metered-сообщения.

## 6. Open questions

1. **Дефолт n**: brief 3, full 1, максимум 100. Обратимо одной константой. Нужен ли другой дефолт?
2. **`CLAUDE_CONFIG_DIR`**: hub не читает его, только `CCTG_PROJECTS_DIR`. Если пользователь запускает Claude Code с нестандартным config dir, надо прописать `CCTG_PROJECTS_DIR` в `.env`. Добавлять автоопределение?
3. **Живой smoke** (перенесён из TASK-008, исполнитель в песочнице сделать не может): после merge пользователь или оркестратор запускает `cctg hub` с реальным `.env`, шлёт `/brief` и `/full 1 <prefix>` в General, перезапускает hub и убеждается, что команда не повторилась. Там же замер working set под живым long poll. Не блокирует merge?
4. **Субагенты в ответе**: используются `render_brief`/`render_full` без `*_with_subagents`, строки `↳ <type> <id>` без тела субагента. Разворачивание (чтение `subagents/agent-*.jsonl` + `.meta.json`) оставлено TASK-015. Согласны?

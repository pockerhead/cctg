# TASK-018 PLAN_FINAL: hook spool, then the multi-slot soak

## 1. Summary

Две части, в этом порядке. (A) Спул хука: `cctg hook`, не доставивший `SessionStart`/`SessionEnd`, кладёт событие файлом в `<state>/spool/<session>/<nanos:020>-<event_id>.json` (`state` = абсолютный `CCTG_STATE_DIR`, иначе `<home>/.cctg`), записанным через `.tmp` + `write_all` + `sync_all` + `rename`; следующий хук той же сессии и агент этой сессии после каждой регистрации сначала досылают спул по порядку (at-least-once, hub дедуплицирует по `event_id`), поэтому сессия, стартовавшая при выключенном hub, становится видимой. Stop/UserPromptSubmit/Subagent не хранятся (нет текста), секрет не входит в `HookPost`, границы 16 файлов на сессию, 256 всего, 16 KiB на файл, 24 ч. Изменений в hub нет: поздний `SessionStart` он принимает как обычный. (B) Soak: `crates/cctg/tests/soak.rs` (`harness = false`, запускается только с `-- --ignored`) гоняет пять сессий настоящими процессами `cctg hook`/`cctg agent` (дублёры `claude.exe` строят настоящее дерево процессов) против настоящих `serve_hooks`, `serve_agents`, `Slots`, `Scheduler`, `updates::poll` и фейкового Telegram; проверяет три темы, маршруты, 429, приоритет permission, один разделитель, удаление `forum_topic_edited` и `registry.json` с точными наборами ключей. Живой режим (`CCTG_SOAK_LIVE=1`) запускает человек/оркестратор при остановленном hub; он трогает только свои темы и в конце удаляет их. Плюс исправление утечки `%TEMP%/cctg-test-*-slots` в тестовом `TempDir`. Готовая, собранная и проверенная реализация лежит в `maw/tasks/in_progress/TASK-018/scratch/reviewer2/ws/`; исполнитель применяет патч и сверяет хеши.

## 2. Implementation steps

### 2.0 Применить эталон

Из корня репозитория (HEAD `818dd90` или любой коммит, где продуктовый код равен `114e786`; проверить `git diff --stat 114e786 HEAD -- crates docs Cargo.toml Cargo.lock` = пусто):

```
git apply --check maw/tasks/in_progress/TASK-018/scratch/reviewer2/task018.patch
git apply maw/tasks/in_progress/TASK-018/scratch/reviewer2/task018.patch
bash maw/tasks/in_progress/TASK-018/scratch/reviewer2/verify_hashes.sh
```

`verify_hashes.sh` считает sha256 LF-байтов (CR вырезается) и должен напечатать `OK` для всех 10 файлов, exit 0. Хеши (`scratch/reviewer2/hashes.txt`):

```
155230c34586981fa8f2bf8443d0374a66f004dff863308e429a2a99f6d300cf  crates/cctg/Cargo.toml
60495de341506530a4b31568ef6c446a02913106f7fc975f1c653245174e6aef  crates/cctg/src/agent.rs
7e35874935003f513318d074ac1cab4818db4fa7d6279780ab35a7023828b2ec  crates/cctg/src/device.rs
4099a4bd8c604c3b27f9eaceb2c43220a50a4692713dab13fc5fc00ceea38137  crates/cctg/src/hook.rs
98f5d77dc519351d77d5448b3be59709380c73fbb838d2222c2c4eb82eb97f9a  crates/cctg/src/hub/testdir.rs
f21f46ff6f0c1231ac52932d21643fb6d025eddc4bd52b2a91a392ef7cdc7256  crates/cctg/src/lib.rs
1a098f318a99dfac386813dadb8ba5ccf32bdcd90981ec16a29fd7a23a06a4bc  crates/cctg/src/spool.rs
a118a889d26c085ba732634ead94bac84a95ca63556ca65dd3908fb12966dd6a  crates/cctg/tests/soak.rs
2aa48f890e622322cdb9ad539a057efb5bd3dca3592afabb12a730a6e0ae4726  crates/cctg/tests/spool_e2e.rs
24d7ecfa85dc179aaa01601e58dbab094c7831e62409af0d288499362befb100  docs/soak.md
```

Не применять `scratch/planner/task018.patch`: в нём нет исправлений этого ревью. Если патч не ложится, скопировать 10 файлов из `scratch/reviewer2/ws/` по тем же путям и снова запустить `verify_hashes.sh`. Ниже по шагам, что именно в каждом файле и зачем (чтобы ревьюер кода мог сверить).

### 2.1 `crates/cctg/src/device.rs`: общий путь состояния

- `use crate::hub::config::{..., STATE_VAR}`; `pub const DEVICE_STATE: &str = ".cctg";`; строка про `CCTG_STATE_DIR` в module doc.
- `DeviceConfig.state_dir: Option<PathBuf>` = `CCTG_STATE_DIR` (process env, затем `device.env`), только если путь абсолютный; иначе `home_dir(&var)/.cctg`; без home и без абсолютного значения `None`. `home_dir` принимает `&impl Fn`.
- Зачем: хук запускается в папке сессии, относительный путь писал бы туда; хук и агент должны видеть один спул.
- Тест `the_state_dir_is_absolute_or_under_home`.

### 2.2 `crates/cctg/src/spool.rs` (новый) и `crates/cctg/src/lib.rs` (`pub mod spool;`)

- Константы `MAX_PER_SESSION = 16`, `MAX_FILES = 256`, `MAX_FILE = 16 KiB`, `MAX_AGE = 24 h`, `TMP_GRACE = 60 s`.
- `SpoolError { NotKept, BadSession, Full, TooLarge, Io(ErrorKind) }`, фиксированные тексты без пути и id.
- `keeps`: только `SessionStart | SessionEnd`. `session_dir`: id только `[A-Za-z0-9_-]{1,128}`.
- `save(root, post, now)`: проверки kind/id/размера; `prune` (удаляет истёкшие `.json` и `.tmp` старше `TMP_GRACE` во всех сессиях, считает остальное) `>= MAX_FILES` или файлов сессии `>= MAX_PER_SESSION` → `Full`; затем `create_dir_all`, `OpenOptions::new().write(true).create_new(true)` для `<name>.tmp`, `write_all`, `sync_all`, закрыть, `rename` в `<name>.json`; при ошибке после создания `.tmp` удалить его. Шаблон тот же, что `RegistryStore::save`.
- Границы без межпроцессного lock (решение ревью, см. 5): каждый сохраняющий добавляет не больше одного файла после проверки, поэтому k одновременных хуков превышают границу максимум на k-1 файлов по <16 KiB, а следующий `save` видит `Full`. Это записано в module doc.
- `pending(root, session, now)`: удаляет stale `.tmp` своей сессии, затем читает `.json` по возрастанию времени; истёкшие, нечитаемые, чужой сессии, не Start/End и больше `MAX_FILE` удаляет; свежий `.tmp` не трогает.
- `async replay(root, session, addr, secret, deadline) -> Result<usize, PostError>`: по порядку `hook::post` с остатком общего deadline, удаление файла только после 2xx (`NotFound` при удалении игнорируется: конкурирующий replay), на первой ошибке стоп, хвост остаётся; пустой каталог сессии удаляется.
- Юнит-тесты (все в модуле): kinds без текста и секрета, порядок и id, границы (сессия, всего, размер, истечение освобождает место), `concurrent_saves_overshoot_a_bound_by_at_most_one_file_each` (8 потоков через `Barrier` при 15 из 16: итог в `16..24`, все файлы целые, следующий `save` = `Full`), плохие id, битые/чужие/старые файлы и свежий против мёртвого `.tmp`, replay по порядку и повтор отбрасывается hub-ом, упавший replay сохраняет хвост, без hub ничего не теряется.

### 2.3 `crates/cctg/src/hook.rs`: replay перед своим событием

- `run`: `spool = config.state_dir.map(spool::dir)`; `deliver(spool, addr, secret, &post, timeout)`: один deadline на всё; сначала `spool::replay` своей сессии (`?`: при отказе своё событие не отправляется, иначе обогнало бы сохранённый start), затем своё событие на остаток. При ошибке `spool::save` и одна из трёх фиксированных строк stderr: `hook event not delivered; kept for the next hook` / `hook event not delivered` (NotKept) / `hook event not delivered and not kept` с текстом `SpoolError`. Exit всегда 0, stdout пуст.
- Тесты: `kept_events_of_the_session_go_before_its_own`, `a_failed_kept_event_stops_the_hook_within_its_budget` (молчащий hub, одно соединение), `a_refused_kept_event_keeps_the_own_event_back` (hub отвечает `HTTP/1.1 503 ...\r\n...` с явными `\r\n`-escape и проверкой `assert_eq!(result, Err(PostError::Status(503)))`, ровно один запрос).

### 2.4 `crates/cctg/src/agent.rs`: replay после регистрации, не больше одного

- `REPLAY_TIMEOUT = 5 s`; `LinkConfig.replay: Option<Replay { spool, hook_addr }>` (без изменения wire, без новой capability); `run_stdio` заполняет из `DeviceConfig`.
- `spawn_replay(&config, &mut running: Option<JoinHandle<()>>)`: если прошлый replay ещё идёт (`!is_finished()`), новый не запускается; иначе `tokio::spawn` одного `spool::replay` с логом только счётчика или фиксированного предупреждения. В `run`: `let mut replaying = None;` и вызов после каждой успешной регистрации. `use tokio::task::JoinHandle;`.
- Тест `one_replay_runs_at_a_time`: молчащий hook-endpoint, два вызова `spawn_replay` подряд, ровно одно соединение.

### 2.5 `crates/cctg/src/hub/testdir.rs` (только `#[cfg(test)]`): утечка temp-каталогов

- `Drop` повторяет `remove_dir_all` до исчезновения каталога (до 50 раз по 20 мс). Причина: saver актора `Slots` живёт дольше тела теста и может переименовать `registry.json` в каталог во время удаления, тогда `remove_dir_all` падает на непустом каталоге. После исчезновения каталог никто не пересоздаёт: `RegistryStore::save` каталоги не создаёт.
- Первый `TempDir::new` процесса (`Once`) удаляет `%TEMP%/cctg-test-*` старше 1 ч: так уходят каталоги убитых прогонов (у них `Drop` не выполнялся).
- Тест `a_save_in_flight_during_the_drop_leaves_no_directory` (только Windows, детерминированный): открытый без `FILE_SHARE_DELETE` `registry.json.tmp` мешает удалению, через 100 мс закрывается и переименовывается в `registry.json`, как save в полёте; каталога после `drop` нет. С одним `remove_dir_all` тест падает (M18) с той же картиной, что у найденных утечек.
- Продуктовый код `Slots` не меняется.

### 2.6 `crates/cctg/tests/spool_e2e.rs` (новый): настоящие процессы

1. `a_missed_session_start_reaches_the_hub_before_the_next_hook`: hub нет → один файл, stderr без id и пути; hub есть → `UserPromptSubmit` доставляет сохранённый start байт в байт, потом свой; вернувшийся файл hub отбрасывает.
2. `the_spool_holds_no_text_and_is_bounded`: hub 503; Stop/UserPromptSubmit не хранятся; 18 start/end дают 16 файлов с точным набором ключей `HookPost`, без текста и секрета.
3. `the_agent_delivers_its_sessions_kept_start_when_it_registers`.
4. `a_silent_hub_costs_a_hook_one_budget_however_much_is_kept` (новый): 15 файлов, hub принимает и молчит; настоящий `cctg hook SessionEnd` укладывается в 1.5 с, одно соединение, свой end сохранён последним.

### 2.7 `crates/cctg/Cargo.toml`

`[[test]] name = "soak"`, `harness = false`, с комментарием в две строки.

### 2.8 `crates/cctg/tests/soak.rs` (новый)

Структура: `main` (роли `launch`/`claude`, без `--ignored` печатает `soak: skipped`), `launch`, `stand_in`, `Sim`, `Tg` (+ `Updates`), `Soak::{start_hub, say, press, launch, registry, spool_files}`, `scenario`, `soak`. Отличия от эталона планировщика:

- `impl Drop for Hub` абортирует задачи hub; `hub.stop()` заменён на `drop(hub)`.
- `soak()` держит `Soak` в `Arc`, `scenario` запускается `tokio::spawn`; после него всегда: (live) удаление созданных тем, 4 с на выход дублёров, удаление `%TEMP%/cctg-soak-<pid>`; затем `resume_unwind` паники сценария или отчёт; неудалённые темы печатаются в stderr и валят тест.
- Live: preflight `getMe` + `getChatMember` (`creator` или `can_manage_topics && can_delete_messages`); `delete_topics` делает прямой `POST https://api.telegram.org/bot<token>/deleteForumTopic` через `reqwest` (новых методов `BotApi` нет), новейшие темы первыми, до 5 попыток с `retry_after`, ошибки не читаются и не печатаются (в них URL с токеном).
- Live: `synthetic(op)` = `React` по id `>= SYNTHETIC_MESSAGE (2_000_000_000)` и `AnswerCallback` с `query_id` на `soak-`; такие операции транспорт отвечает сам (`outcome = "synthetic"`), в Telegram не уходят.
- Service messages считаются только для трёх своих тем; fake: `shown == accepted editForumTopic`; live: `shown > 0` (иначе работает другой читатель `getUpdates`); `left` пуст.
- Permission: утверждение и отчёт говорят то, что доказано: все строки всплеска были в транскрипте A2 до записи запроса, `behind_a2 > 0` из них пришли в тему после prompt.
- `registry.json`: точные ключи верхнего уровня (`pids, seq, sessions, slots, subagents, version`), `version = 1`, `seq > 0`, `subagents = {}`; у каждого слота точный набор ключей (без `buffer`) и значения host/folder_name/folder_key/ordinal/topic_id/current_session/pending_separator/applied_title/applied_icon; у A1/A2/B1/A5 точные ключи, host, kind, slot, ended, `claude_pid` дублёра, `transcript_path`, `title = null`, `stream.offset == длина транскрипта`, `stream.calls = []`, `receipts` целые; у вложенного точные ключи (без `stream`), parent A1, slot 0, ended, путь транскрипта, свой pid, блок (`header`, `thread_id = t_a`, `message_id`, `running/sending = false`, `pending = null`); пять разных `seen`; `pids` ровно A2/B1/A5.
- Отчёт считает send / permission / stream / document / edit / react / callback / createForumTopic / editForumTopic / deleteMessage отдельно, 429, "answered locally", задержки permission, пик бакета; правки не сравниваются с 20/мин.

### 2.9 `docs/soak.md` (новый)

Назначение, устройство, fake-прогон (обязательный), живой прогон (ручной): hub остановлен, проверка прав, только свои темы, автоматическое удаление трёх тем даже при падении, синтетические операции не уходят в Telegram, известные ограничения спула (ответы хода при выключенном hub и чужие `SessionEnd` не досылаются); шаблон отчёта.

## 3. Test plan

Сборка (хост с малой памятью): один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз; каталог удалить в конце.

```
export CARGO_TARGET_DIR="$TEMP/cctg-018-target" CARGO_PROFILE_DEV_DEBUG=0
cargo test -j 1 --offline -p cctg --lib -- spool hook:: agent:: device:: testdir
cargo test -j 1 --offline -p cctg --test spool_e2e
cargo test -j 1 --offline -p cctg --test hook_cli
cargo test -j 1 --offline -p cctg --test soak -- --ignored     # три раза подряд
cargo fmt --all --check
cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast -j 1 --offline
ls -d "$(cygpath -u "$TEMP")"/cctg-test-* 2>/dev/null | wc -l   # 0 после прогона
ls -d "$(cygpath -u "$TEMP")"/cctg-soak-* 2>/dev/null           # нет каталогов этого прогона
rm -rf "$TEMP/cctg-018-target"
```

Ожидаемо (получено на эталоне, `scratch/reviewer2/`):

- `workspace_test.txt`: exit 0; `cctg` lib 407 passed (1 ignored, был и раньше), `spool_e2e` 4, `hook_cli` 7, `stream_e2e` 11, остальные цели зелёные, `soak: skipped`. `cctg-test-*` в `%TEMP%` после прогона: 0 (до фикса 8 обычных запусков lib оставили 12 каталогов с одним `registry.json`).
- Soak fake: `soak_runs.txt`, три прогона подряд exit 0, `soak: ok`, 15.3-15.7 с сценария; 102-104 вызова; ровно 3 createForumTopic, 1 разделитель; 13-14 service messages, все удалены; 2 x 429, после каждого вся очередь молчит `retry_after`, один успешный повтор; permission A 31-46 мс (другая тема), A #2 189-201 мс (своя тема), 28 строк всплеска A #2, записанных до запроса, ушли после prompt; пик бакета 92%, минимальный зазор 30 мс = `min_gap`.
- Мутации (`mutations.out.txt`, скрипт `mutations.py`): 20, убито 19. M1-M14 планировщика убиты снова (M8 переписан: прежний вариант просто не компилировался, теперь убит `the_agent_delivers_its_sessions_kept_start_when_it_registers`; M3 теперь убит по верной причине, 503). Новые: M16 (мёртвый `.tmp` не удаляется), M17 (второй replay), M18 (`TempDir` без повтора), M19 (вложенный блок остаётся `running`, ловит только новый валидатор реестра), M20 (старые LF-байты в тесте 503) убиты. M15 (без `sync_all`) выживает ожидаемо: потерю питания тестом не проверить.
- Не проверяется автоматически: живой прогон (настоящий Telegram) и долговечность `sync_all` при потере питания (M15).

Живой прогон (не делает исполнитель; делает оркестратор или пользователь после влития, штатный `cctg hub` остановлен), из корня репозитория в Git Bash:

```
CARGO_TARGET_DIR="$TEMP/cctg-live-target" CARGO_PROFILE_DEV_DEBUG=0 CCTG_SOAK_LIVE=1 CCTG_SOAK_REPORT="$PWD/soak-live.md" cargo test -j 1 -p cctg --test soak -- --ignored
```

Успех: exit 0 и `soak: ok`; в группе не осталось тем `[soakbox]` (тест их удалил); отчёт в `soak-live.md`. Если тест напечатал `topics of this run not deleted`, удалить эти id руками. Потом `rm -rf "$TEMP/cctg-live-target"` и запустить hub.

## 4. Rollout notes

- Миграций нет, `wire::VERSION` не меняется, hub не меняется. Новый необязательный параметр устройства `CCTG_STATE_DIR` (абсолютный; относительный игнорируется); по умолчанию спул в `~/.cctg/spool/`. Хуки и агенты старой версии просто не пишут и не читают спул.
- Спул содержит папку и путь транскрипта сессии (как `registry.json`), без секрета и текста. Не логируется.
- Известные ограничения (решения оркестратора): ответы `Stop` ходов, прошедших при выключенном hub, не восстанавливаются; `SessionEnd` сессии, закончившейся при выключенном hub, лежит в спуле, но досылать его некому (слот выглядит занятым до переиспользования pid); после рестарта hub повтор неудалённого файла (сбой между 204 и удалением) проходит заново, для start живой сессии это безопасно. После `/clear` агент держит старый `CLAUDE_CODE_SESSION_ID` и досылает спул старой сессии; спул новой сессии досылает её следующий хук.
- Soak в обычном `cargo test --workspace` не запускается (`soak: skipped`). Живой режим требует остановленного hub: он подтверждает общую очередь `getUpdates`, сообщения, написанные боту за время прогона, штатный hub не увидит.
- Остатки прошлых прогонов планировщика: `%TEMP%/cctg-soak-{19264,24948,25960,36048,38816}` (11:01-11:02, упавшие мутации до этого ревью, в каждом копия бинарника) можно удалить руками; новый код удаляет свой каталог и при падении.

## 5. Review notes

Проверенный контрпример (до оценки): PLAN_V2 (п. 6) утверждал, что живой режим может удалить служебное сообщение чужой темы. `Slots::on_control` удаляет только при `registry.slot_by_topic(thread)`, а реестр soak свежий, поэтому не подтвердилось (`scratch/reviewer2/disconfirmation.md`). По пути найден новый дефект: тест 503 в `hook.rs` отвечал голыми LF (многострочный литерал; Rust и CRLF-литерал превращает в LF), клиент получал `BadResponse`, тест проходил не по той причине.

Что из PLAN_V2 воспроизвелось и исправлено (минимально):

| # | Находка | Статус | Изменение |
|---|---|---|---|
| 1 | решения оркестратора не внесены | да | Stop не хранится и чужие `SessionEnd` не досылаются (уже так в коде, записано в docs и rollout); удаление живых тем и фикс утечки сделаны (п. 7, 11) |
| 2 | гонка границ между процессами | да, гонка есть | lock не добавлен: правило "перебор не больше k-1 файлов" доказано тестом с 8 потоками и записано в doc; lock стоил бы ожидания внутри бюджета хука и режима отказа "застрявший lock" ради границы, которая защищает только место на диске |
| 2 | `.tmp` после падения не удаляется | да | `drop_stale_tmp` в `prune` и `pending`, `TMP_GRACE` 60 с, тест, мутация M16 |
| 3 | нет fsync | принято без воспроизведения (питание не выключить) | шаблон `RegistryStore::save`, `create_new` + `sync_all`; M15 честно выживает |
| 4 | wall-clock хука не проверен | да, теста не было | `a_silent_hub_costs_a_hook_one_budget_however_much_is_kept` на настоящем `cctg hook` |
| 5 | replay агента не single-flight | путь в коде есть (reconnect внутри 5 с) | проверка `is_finished()`, тест, M17 |
| 6 | live трогает чужое | частично: удаления чужих нет (контрпример), но синтетические `setMessageReaction`/`answerCallbackQuery` уходили в настоящий чат и чужие service messages попадали в проверку | синтетика отвечается локально, проверка только своих тем, непустота в live |
| 6 | live читает общую очередь | принято решением оркестратора (hub остановлен) | PLAN_V2 предлагал не читать `getUpdates` вообще; отклонено: тогда живой прогон не проверяет пункт про `forum_topic_edited` |
| 7 | нет удаления живых тем | да | прямой `deleteForumTopic`, preflight прав |
| 8 | нет cleanup при panic | да | `Drop for Hub`, сценарий в отдельной задаче, cleanup до `resume_unwind` |
| 9 | реестр не поле за полем | да | точные наборы ключей и значения, M19 (блок остаётся `running`) убит только новым валидатором |
| 10 | доказательство приоритета permission | формулировка была сильнее факта | утверждение и отчёт говорят "записаны до запроса, отправлены после prompt"; очередь внутри scheduler отдельно доказывает существующий unit-тест scheduler и мутация M11 |
| 11 | утечка `cctg-test-*-slots` | да: 12 каталогов за 8 обычных прогонов lib, 20 от одного убитого | фикс в тестовом `TempDir` (повтор удаления + уборка старше 1 ч) вместо `Rig::shutdown` в ~75 тестах и поля в продуктовом `Slots`: тот не лечит убитые прогоны и в разы больше диффа; тест гонки, M18 |
| 12 | `scenario` 560 строк | отклонено | фазы делят ~20 локальных переменных, разбиение добавит структуру контекста без нового покрытия; панико-безопасная обёртка уже ограничивает последствия падения |
| 13 | ссылки на лимиты Telegram | согласен | без изменений |

Решения записаны в `log.jsonl` (stage plan-reviewer-2). PCTX_PROPOSALS.md не менялся.

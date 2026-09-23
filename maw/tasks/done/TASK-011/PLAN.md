# PLAN — TASK-011: hub, слотовый реестр и жизненный цикл тем

Stage: planner (claude/opus, effort=medium). Пути от корня репозитория `C:/Users/user/dev/cctg`, HEAD `dbc073a`.
`T` = `maw/tasks/in_progress/TASK-011`. `REF` = `T/scratch/planner/ws`: полная копия workspace с этим планом, собранная и проверенная:

- `cargo fmt --all -- --check` чисто, `cargo clippy --workspace --all-targets --offline -- -D warnings` чисто.
- `cargo test --workspace --offline`: 221 passed, 0 failed, 1 ignored (ignored это старый изолированный config-тест). Лог: `T/scratch/planner/workspace_test.txt`. На HEAD 189 тестовых функций; новых 32 (registry 21, slots 9, sessions 1, `tests/slots_logs.rs` 1).
- Флейки: `hub::slots hub::registry hub::sessions` 8 прогонов, `--test slots_logs` 5 прогонов, 0 падений (`T/scratch/planner/flake.out.txt`).
- 12 мутаций (`T/scratch/planner/mutate.py`, итог `mutations.out.txt`): 11 KILLED, 1 SURVIVED и она эквивалентна (см. раздел 4).
- Патч `T/scratch/planner/task011.patch` (6 файлов, +2859/−29) проверен: `git apply --check` в корне репозитория проходит; на свежем клоне HEAD с `core.autocrlf=true` `git apply` плюс `verify_hashes.sh` дают 6 из 6 `OK`.

Telegram вызывался один раз и только на чтение: `getForumTopicIconStickers` скриптом `T/scratch/planner/icon_probe.py` (токен читается из `.env` внутри скрипта и нигде не печатается; сохранены только emoji и id в `icon_stickers.json`, 112 штук).

## 1. Understanding

Что есть сейчас (всё в `crates/cctg/src`):

- `wire.rs` (TASK-010). `Register { session_id, host, cwd }` (112-118) приходит от агента. `HookPost { host, session_id, cwd, transcript_path, event }` (322-355) от хука. `HookEvent::SessionStart { source, claude_pid, parent_claude_pid }` (361-370) уже несёт pid своего claude и pid следующего claude-предка: этого хватает, чтобы hub сам решил вложенность по реестру pid, без изменения протокола. `SubagentStart/Stop` (385-398) несут `agent_id`, `agent_type`, родительский `session_id` в `HookPost`.
- `hub/ingress.rs`. `AgentEvent::{Registered{conn, register, to_agent}, Message{conn, msg}, Disconnected{conn}}` (52-69). `serve_agents` и `serve_hooks` отдают события в два bounded-канала по 256. Урок TASK-010 QA: потребитель не должен ждать Telegram, иначе запись агента упрётся в 5 с и сообщение потеряется.
- `hub/mod.rs`. `run()` (88-149): конфиг, `OffsetStore`, bind listener-ов, `getMe`, `getChatMember` + `check_topic_rights`, предупреждение без `can_delete_messages` (114-116), `Scheduler`, `commands::serve` c `ProjectsDir`, ingress, временный `drain_ingress` (69-86), потом `updates::poll` с `route_inbound` (56-67), где `Routed::Service` сейчас выбрасывается.
- `hub/scheduler.rs`. `Op::{CreateTopic, EditTopic, Delete, Send, ...}` (35-70); topic-lane не тарифицируется; `Outbox::submit` возвращает `oneshot` c `Delivery = Result<Outcome, ApiError>`; 429 обрабатывает сам планировщик. `Outcome::Topic(ForumTopic)` отдаёт `message_thread_id`.
- `hub/api.rs`. `create_forum_topic`, `edit_forum_topic`, `delete_message`, `get_forum_topic_icon_stickers` (284-317) уже есть. `ApiError::Telegram { code, description }` несёт текст Telegram (`TOPIC_ID_INVALID` и т.п.).
- `hub/updates.rs`. `Routed::Service(ServiceMessage { kind, message_id, thread_id })` (49-62), `ServiceKind::TopicEdited` распознаётся до allowlist (94-105).
- `hub/sessions.rs`. Трейт `TranscriptLocator::locate(thread_id, prefix)` (36-44) и `ProjectsDir` (46-128). По TASK-009 задача меняет реализацию, не трейт.
- `hub/commands.rs`. `locate_notice` (205-237) переводит `LocateError` в текст.
- `hub/offset.rs`. Образец атомарной записи: temp + `sync_all` + `rename` (64-73). `std::fs::rename` на Windows заменяет существующий файл (MoveFileEx с REPLACE_EXISTING), этим уже пользуется offset.

Чего нет: реестра слотов, тем, иконок, разделителей, удаления `forum_topic_edited`, сверки после рестарта. TASK-012 (хук) и TASK-013 (агент) ещё не написаны, поэтому весь поток проверяется фейками.

Факты, проверенные в этой стадии:

- Иконки. `getForumTopicIconStickers` отдал 112 стикеров, зелёного круга среди них нет. Выбраны: alive `5312016608254762256` (⚡️), dead `5408906741125490282` (🏁), waiting `5377316857231450742` (❓), no channel `5357121491508928442` (👀, «видно, но говорить нельзя»).
- `ai-title` в реальных транскриптах повторяется сотни раз с одним и тем же текстом, первое вхождение на байтах 59 KB..683 KB (выборка 8 самых больших файлов в `~/.claude/projects`, 6..96 MB). Значит, хватает читать голову файла (4 MiB) и брать первое вхождение через существующий `transcript::ai_title`.
- Ошибки `channels.editForumTopic` по докам MTProto: `TOPIC_ID_INVALID` («The specified topic ID is invalid») и `TOPIC_NOT_MODIFIED` («The updated topic info equals current info»), https://core.telegram.org/method/channels.editForumTopic. `TOPIC_NOT_MODIFIED` по практике считается успехом (идемпотентное повторное применение, ccgram issue #197: https://github.com/alexei-led/ccgram/issues/197). Там же лимит заголовка: «maximum UTF-8 length: 128».
- Сервисные сообщения правки темы это `messageActionTopicEdit` (https://core.telegram.org/api/forum), в Bot API они приходят как `forum_topic_edited` (проверено в CLAUDE.md).

## 2. Approach

Два новых модуля и точечная проводка.

**`hub/registry.rs`, чистая логика без IO и async.** Модель из закона домена hub: `slots: Vec<Slot>` (`SlotId` = индекс, слоты не удаляются, значит id стабилен), `sessions: BTreeMap<session_id, SessionEntry { host, kind: TopLevel | Nested{parent: Option}, slot, transcript_path, claude_pid, title, ended, seen }>`, `subagents: agent_id -> { parent_session, slot }`, `pids: "<host>/<claude_pid>" -> session_id`. Несериализуемые поля: `agent` (номер соединения), `waiting`, `busy`, `failed`.

- `folder_key(cwd)`: снять `\\?\` (и `\\?\UNC\` → `\\`), `\` → `/`, убрать хвостовые `/`, для пути с буквой диска или UNC сделать `to_lowercase`. POSIX-пути не трогаются. `folder_name(cwd)`: последний компонент в исходном написании, для заголовка. Symlink/junction/8.3 не разрешаются, это TASK-012 на устройстве.
- Выбор слота для top-level сессии по порядку: слот, который она уже держит; её прежний слот, если он свободен (resume остаётся в своей теме); слот прежней сессии того же `host + claude_pid` в той же папке (`/clear` меняет id в том же процессе; прежняя сессия помечается завершённой); первый свободный ordinal папки; новый ordinal = max + 1. «Свободен» значит «нет текущей сессии или она `ended`».
- Вложенность решается на hub: `parent_claude_pid = None` → TopLevel; pid есть в `pids` → `Nested { parent: Some }` и ссылка на слот родителя (для вложенного во вложенный это тот же слот); pid нет → `Nested { parent: None }` без слота (NestedUnknownParent из контракта TASK-003); pid указывает на саму сессию → TopLevel (протухший `CLAUDE_PID`, защита из TASK-003). `pids` пишется на SessionStart и чистится на SessionEnd.
- Субагенты: `SubagentStart/Stop` с непустым `agent_type` записываются как `agent_id -> { parent_session, slot родителя }`. Пустой `agent_type` это внутренняя работа Claude Code, отбрасывается. Тем не создают.
- Состояние слота из текущей сессии: нет сессии или `ended` → Dead; нет агента → NoChannel; `waiting` → Waiting; иначе Alive. `waiting` ставит `AgentMsg::PermissionRequest`, снимают Stop, UserPromptSubmit, SessionStart и отключение агента (вердикт добавит TASK-014).
- Заголовок `topic_title(host, folder, ordinal, label)`: `[host] folder #N · label`, `#N` только при ordinal > 1, label это ai-title или первые 8 символов id. Длина считается `transcript::telegram_len` (UTF-16 единицы, не меньше числа символов) и не превышает 128. При нехватке места сначала режется label, потом folder (с `…`), host ограничен 32; `[host]` и ` #N` остаются всегда. Переводы строк и управляющие символы превращаются в пробелы.
- Разделитель `── session <short> · new | resumed ──` (`resumed` при `source == "resume"`) ставится в `pending_separator`, только когда текущая сессия слота меняется на другую. Первая сессия нового слота разделителя не получает, иначе в критерии 1 в теме было бы два разделителя.
- Работа с Telegram считается одним диффом `topic_work(icons, edits)`: для каждого не-`busy` слота желаемые имя и иконка против `applied_title/applied_icon`. Нет темы → `Create`; есть `pending_separator` → `Separator`; расхождение → `Edit` только с изменившимися полями. Результаты: `topic_created`, `topic_edited` (и для `TOPIC_NOT_MODIFIED`), `topic_invalid(slot, thread_id)` (забывает тему только если слот всё ещё привязан к этому thread, поэтому повторные и поздние отчёты не плодят замен), `topic_failed` (запоминает желаемое, не повторяет до `retry_failed`).
- `SessionEnd` только ставит `ended`; операции закрытия темы в коде нет вовсе (в `Op` её нет и не добавляется).
- Рост: завершённые сессии, которые не текущие ни в одном слоте, сверх 1024 удаляются по возрастанию `seen` вместе с их субагентами и pid.
- `RegistryStore` в `<CCTG_STATE_DIR|.cctg>/registry.json`: `load` (нет файла → пустой реестр; не парсится, чужая версия или ссылка на несуществующий слот → ошибка без цитирования содержимого, hub не стартует), `encode` (pretty JSON), `save` (temp `registry.json.tmp` + `sync_all` + `rename`, как `offset.rs`). После загрузки `after_restart` сбрасывает соединения, `waiting`, `busy`, `failed`.

**`hub/slots.rs`, актор `Slots`, единственный владелец реестра.** `select!` по четырём входам: `AgentEvent`, `HookPost`, `Control::TopicEdited` из poll, `Done` (ответы собственных задач), плюс таймер. После каждого события `pump()`: `topic_work` → `Outbox::submit` → отдельная задача ждёт `oneshot` и шлёт `Done` обратно. Актор никогда не ждёт Telegram (урок TASK-010 QA).

- Агент известной сессии сразу привязывается. Агент неизвестной сессии ждёт свой SessionStart `hook_wait = 10 s` (порядок между двумя каналами `select!` не гарантирован); не дождался → `adopt` как top-level. UserPromptSubmit/Stop неизвестной сессии тоже её регистрируют: это сессия, стартовавшая при выключенном hub (TASK-018).
- Грейс после старта `grace = 45 s` (больше 30 с максимального backoff агента): правки тем ждут, создание тем и разделители идут сразу. Так рестарт hub не перекрашивает все живые темы в «нет канала» и обратно.
- Повтор неудачных вызовов раз в `retry_every = 60 s`.
- `forum_topic_edited` в теме слота удаляется `Op::Delete`, если у бота есть право удаления (`creator` или `can_delete_messages`). Нет права → не пытаемся вовсе, единственный лог это существующее предупреждение на старте. Первая ошибка удаления в работе → один `warn`, дальше `debug`.
- ai-title: Stop/UserPromptSubmit top-level сессии без заголовка → `spawn_blocking` читает до 4 MiB `transcript_path` → `set_title` → дифф переименует тему. Для удалённого устройства файла нет, остаётся короткий id.
- Сохранение: снимок реестра в `watch`, отдельная задача пишет последний снимок (промежуточные схлопываются), 3 попытки как у offset, потом `warn` без пути.
- Вид для `/brief`: `watch<Arc<TopicView>>`, `TopicView = BTreeMap<thread_id, (session_id, transcript_path)>`.

**Проводка.** `SlotLocator` в `sessions.rs`: в теме слота без префикса отдаёт текущую сессию слота; General, префикс и незнакомая тема идут в `ProjectsDir` как раньше. Новый `LocateError::NoTranscript` (сессия известна только по агенту) с текстом в `commands.rs`. `mod.rs`: загрузка реестра до обращений к Telegram, проверка иконок по `getForumTopicIconStickers` (id, которых Telegram не предлагает, выбрасываются с `warn`; ошибка вызова оставляет умолчания), `Slots` вместо `drain_ingress`, `route_inbound` получает второй канал для `Control`.

Почему так: чистый реестр тестируется без времени и сети, а инварианты критериев («ноль новых тем», «ровно одна замена», «ровно один разделитель») проверяются на нём напрямую. Дифф желаемого против применённого идемпотентен: потерянный ответ, рестарт или 429 не дают двойных операций. Решение о вложенности на hub не требует нового поля в протоколе и оставляет TASK-012 только заполнить `claude_pid/parent_claude_pid`.

Отвергнуто (подробно в `log.jsonl`): `Arc<Mutex<Registry>>` с ожиданием Telegram в цикле ingress; эффекты на каждое событие вместо диффа; поле вложенности в протоколе; разделитель на каждое занятие слота; простой first-free без учёта resume и `/clear`; старт с пустым реестром при битом файле; правки иконок сразу после рестарта; счётчик ожидаемых сервисных сообщений.

## 3. Steps

### Step 0. Изоляция

1. `git status --short -- Cargo.toml Cargo.lock crates` должен быть пуст, ветка `feature/hub-slot-registry`. Иначе остановиться и сообщить.
2. `.env` не открывать, Telegram не вызывать, `~/.claude` не трогать.
3. Cargo по одной команде, target вне репозитория:
   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task011-impl-target'
   ```

### Step 1. Перенести 6 файлов из REF

Основной способ, из корня репозитория:

```bash
git apply --check maw/tasks/in_progress/TASK-011/scratch/planner/task011.patch
git apply maw/tasks/in_progress/TASK-011/scratch/planner/task011.patch
bash maw/tasks/in_progress/TASK-011/scratch/planner/verify_hashes.sh
```

Запасной способ: скопировать `REF/<path>` → `<path>` байт в байт для каждого файла таблицы и запустить тот же скрипт (он убирает `\r` перед хэшированием). Все 6 строк должны быть `OK`. Руками не править.

| Path | Изменение | Критерии | SHA-256 (LF) |
|---|---|---|---|
| `crates/cctg/src/hub/registry.rs` | новый: иконки, `folder_key`/`folder_name`, `topic_title`, `separator`, `Slot`/`SessionEntry`/`SubagentEntry`/`Registry`, выбор слота, вложенность, `topic_work` и `topic_*`, `prune`, `RegistryStore`; 21 тест | 1-9 | `83817a6bce66f0ef0f7b3319110b364d780a6689a36bd80f626c6a498d8b982a` |
| `crates/cctg/src/hub/slots.rs` | новый: `Options`, `Control`, `read_title`, актор `Slots` (`new`, `run`, обработчики, `pump`), `save_loop`; 9 тестов на фейковом `Transport` через настоящий `Scheduler` | 1-4, 6-8 | `2e3bd1220fd7db218a51104a3039f707cc76f4670da24b4ac0449729093d5a6e` |
| `crates/cctg/src/hub/sessions.rs` | `SlotLocator` (view + fallback `ProjectsDir`), `LocateError::NoTranscript`, модульный комментарий, 1 тест | `/brief` в теме слота | `f5cf0be28c554c8219fbcadfbde48a8162448f7364125d05d44f191347e087d5` |
| `crates/cctg/src/hub/commands.rs` | ветка `LocateError::NoTranscript` в `locate_notice`: «У сессии этой темы пока нет известного транскрипта.» | компиляция match | `ca1631b0ce2a6276736a5a08f4f8f50ff1c57975902c21b5186d8794bae03126` |
| `crates/cctg/src/hub/mod.rs` | `pub mod registry; pub mod slots;`; загрузка `RegistryStore` сразу после `OffsetStore`; `can_delete`; `checked_icons`; `Slots::new` + `slots.run` вместо `drain_ingress` (удалён вместе с импортами `HookPost`, `AgentEvent`); `route_inbound(commands, control)` шлёт `Control::TopicEdited`; тест `a_slow_command_does_not_hold_up_polling` передаёт второй канал | 4, 7, 8 | `34423c9f2f36b42f660195194fbc595b5183566afb8cf8005f9f81e708321834` |
| `crates/cctg/tests/slots_logs.rs` | новый отдельный тест-бинарник: глобальный TRACE-подписчик `.without_time()`; удаление без права логируется ровно один раз, работа идёт дальше; в логах нет папки, пользователя из пути, заголовка, пути транскрипта | 7, закон о логах | `d8075b4b58fedfda8c39d836f8ab03e5db8569398c74cfe0a9152eeb5492d83e` |

`Cargo.toml`/`Cargo.lock` не меняются: новых зависимостей нет (`tokio::sync::watch` входит в уже включённую фичу `sync`).

Ключевые места для ревью (по `REF`):

- `registry.rs` `fn lexical`/`folder_key` (96-131): порядок «снять префикс → разделители → хвост → case-fold только для диска/UNC».
- `registry.rs` `fn allocate` (409-467): пять шагов выбора слота; шаг `/clear` помечает прежнюю сессию `ended`.
- `registry.rs` `fn nesting` (482-497) и `session_started` (499-563): известная сессия без `parent_pid` сохраняет свой вид; top-level всегда получает слот (`expect` держится на этом инварианте).
- `registry.rs` `topic_work` (740-788), `topic_invalid` (841-854): сверка `topic_id == Some(thread_id)`.
- `slots.rs` `on_topic_done`: `TOPIC_NOT_MODIFIED` → `topic_edited`; `topic_gone` (`topic_id_invalid`, `topic_deleted`, `message thread not found`) → `topic_invalid`; прочее → `topic_failed`.

### Step 2. Проверка

По одной команде:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task011-impl-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидание: fmt и clippy чисто; 221 passed, 1 ignored. Другое число само по себе не провал, но его надо объяснить; провал это любой failed или новый ignored.

Флейки, без параллельных cargo:

```powershell
1..5 | % { cargo test -p cctg --offline --lib -- hub::slots hub::registry hub::sessions }
1..5 | % { cargo test -p cctg --offline --test slots_logs }
```

После этого `git diff --check`; `git status --short` показывает ровно 6 путей из таблицы.

### Step 3. Коммит

Английский, без trailer-ов `Generated with` / `Co-Authored-By`. Например: `feat(hub): slot registry, topic lifecycle and registry.json (TASK-011)`.

### Test plan (что закрывает каждый критерий)

| Критерий | Тесты | Что проверяется |
|---|---|---|
| 1. Вторая сессия после смерти первой: 0 тем, 1 разделитель | `registry::a_session_after_a_dead_one_reuses_the_slot_with_one_separator`, `slots::one_slot_lives_through_hook_agent_end_and_the_next_session` | другой спеллинг папки, одна тема, ровно один `Send` с `── session bbbbbbbb · new ──` в thread 100, повторный проход ничего не добавляет |
| 2. Параллельные сессии: `#2`, `#3` | `registry::concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first`, `slots::two_live_sessions_make_two_topics_and_waiting_shows` | имена `[box] Project`, `#2`, `#3`; освободившийся `#2` занимается раньше нового `#4`; другая папка и другой host это другие слоты |
| 3. Nested/subagent: 0 тем, ссылка на слот родителя | `registry::nested_runs_and_subagents_get_no_slot`, `registry::a_stale_parent_pid_pointing_at_itself_is_top_level`, `slots::nested_and_subagent_events_make_no_topics` | вложенный в другой папке и вложенный во вложенный указывают на слот родителя; неизвестный предок = nested без слота; субагент с типом записан, без типа нет |
| 4. SessionEnd не закрывает; иконки из списка | `registry::states_map_to_icons_from_the_list`, `slots::one_slot_...` | после SessionEnd ровно один `EditTopic { name: None, icon: DEAD }`; alive/waiting/no-channel/dead; `keep_valid` выбрасывает id вне списка |
| 5. Заголовок ≤128, `[host]` и `#N`, id → ai-title | `registry::titles_fit_and_keep_host_and_ordinal`, `registry::short_id_becomes_the_ai_title`, `slots::the_ai_title_replaces_the_short_id` | кириллица, эмодзи (2 UTF-16 единицы), длинный host, ordinal до `u32::MAX`; реальный файл с `ai-title` даёт `EditTopic` с `[box] Project · Slot registry` |
| 6. Прерванное сохранение; `TOPIC_ID_INVALID` → одна замена | `registry::saves_are_atomic_and_round_trip`, `registry::a_broken_or_foreign_file_refuses_to_load_without_quoting_it`, `registry::a_gone_topic_is_replaced_exactly_once`, `slots::a_deleted_topic_is_replaced_once`, `slots::topic_not_modified_counts_as_applied` | полузаписанный temp не мешает загрузке прежнего файла; дважды и поздно пришедший отказ дают одну `CreateTopic`; тема другого слота не тронута |
| 7. `forum_topic_edited` удаляется; нет права → один лог | `slots::one_slot_...` (удаляется только сообщение из темы слота), `slots::without_the_delete_right_nothing_is_deleted`, `tests/slots_logs.rs` | `Delete{55}` и только он; без права ни одного `Delete`; при отказах ровно одна строка `cannot delete a forum service message` |
| 8. Сессия только от хука = «нет канала», агент позже без второй темы | `registry::a_hook_only_session_is_bound_by_its_agent_later`, `slots::one_slot_...`, `slots::an_agent_before_its_hook_is_bound_without_a_second_topic` | создание с иконкой 👀, потом правка на ⚡️; агент раньше хука ждёт и создаёт одну тему; агент без хука принимается через `hook_wait` |
| 9. `folder_key` | `registry::folder_key_normalizes_windows_spellings_only`, `registry::folder_name_keeps_the_original_spelling`, `registry::folder_spellings_share_one_slot` | `C:\Work\Project`, `c:/work/project/`, `\\?\C:\Work\Project` → один ключ и один слот, заголовок из исходного `Project`; UNC; POSIX чувствителен к регистру |
| 10. Existing tests pass | `cargo test --workspace --offline` | 221 passed, 1 ignored |

Дополнительно: `registry::clear_in_one_process_stays_in_its_slot`, `a_resumed_session_returns_to_its_slot`, `a_failed_call_is_not_repeated_until_retry`, `edits_wait_for_the_grace_but_creations_do_not`, `prompt_hooks_of_an_unknown_session_register_it_and_ask_for_a_title`, `old_ended_sessions_are_pruned_but_current_ones_stay`, `a_restart_forgets_connections_but_keeps_slots`; `slots::a_restart_keeps_slots_and_waits_out_the_grace`; `sessions::a_slot_topic_reads_its_current_session`.

## 4. Risk areas

- **Порядок событий между каналами.** `select!` выбирает готовые ветки случайно, так что регистрация агента может обогнать SessionStart. Это закрыто ожиданием `hook_wait`; тесты написаны с учётом этого (первая версия тестов на этом споткнулась, запись `dead_end` в логе).
- **Сессии без SessionStart считаются top-level.** Если вложенный `claude -p` стартовал при выключенном hub, его агент или Stop придут без признака вложенности и он получит лишнюю тему. Внутри одного запуска hub это невозможно: реестр переживает рестарт, а вложенный запуск, известный до рестарта, остаётся вложенным.
- **Потерянный SessionEnd.** Слот остаётся «нет канала» навсегда, и следующая сессия в папке получит `#2`. Частично лечит правило `/clear` (тот же `claude_pid`). Отдельного протухания нет, это вопрос к TASK-017/018.
- **Лимит 128 для заголовка.** MTProto пишет «maximum UTF-8 length: 128». Если Telegram считает байты UTF-8, кириллический заголовок из 128 символов (до 256 байт) получит 400. Тогда `topic_failed` повторяет попытку раз в 60 с с `warn` на каждой. Это не проверить без записи в Telegram; см. открытый вопрос 1.
- **`/clear` и агент.** Агент регистрируется по `CLAUDE_CODE_SESSION_ID`, унаследованному при спавне. После `/clear` новая сессия займёт тот же слот, но агент может остаться привязан к старому id, и тема покажет «нет канала». Проверяется в TASK-013.
- **Гонка сохранения при завершении hub.** Последний снимок может не успеть записаться, если процесс убить сразу после события. Потеря ограничена последним изменением; атомарность файла от этого не страдает.
- **`registry.json` хранит приватные данные** (имена хостов, пути транскриптов, заголовки). Файл в `.gitignore` (`registry.json`, `.cctg/`), в логи и тексты ошибок ничего из него не попадает (`LoadError` без цитат, логи с короткими id и ordinal, проверено в `slots_logs`).
- **Эквивалентная мутация.** M5 «разделитель и для первой сессии нового слота» выживает, потому что `topic_created` чистит `pending_separator`. Поведение то же, тест убить её не может.
- **Массовый старт.** Создание тем идёт по незатарифицированной topic-lane; лимит Telegram на `createForumTopic` неизвестен (открытый вопрос в CLAUDE.md). 429 обработает планировщик.

## 5. Open questions

1. Как Telegram считает 128 для имени темы: символы, UTF-16 или байты UTF-8? Сейчас UTF-16. Проверка одной записью в Telegram (создать тему с 128 кириллическими символами) не сделана, потому что запись запрещена этой стадии. Предлагаю QA сделать её на живом hub; если это байты, поменять одну константу на счёт `str::len` в `topic_title`.
2. Хост в ключе слота берётся как есть. Хук и агент один бинарник и считают hostname одинаково, но если TASK-012/013 возьмут его из разных источников (`COMPUTERNAME` против `hostname`), регистр может разойтись. Решить в TASK-012: один источник для обоих.
3. Нужен ли «протухший» NoChannel (нет агента и нет хуков N часов → Dead), чтобы потерянный SessionEnd не держал слот? Не входит в критерии; предлагаю решить в TASK-017.
4. Агент должен канонизировать `cwd` так же, как хук (TASK-012), иначе принятая по агенту сессия через junction попадёт в другой слот. Записано в `PCTX_PROPOSALS.md` для TASK-013.

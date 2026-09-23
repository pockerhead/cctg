# PLAN FINAL — TASK-011: hub, слотовый реестр и жизненный цикл тем

Stage: plan-reviewer-2 (claude/opus, effort=medium). Корень репозитория `C:/Users/user/dev/cctg`, HEAD `515a4d3` (в `crates/` и манифестах нет изменений относительно `dbc073a`, на котором строился планировщик).
`T` = `maw/tasks/in_progress/TASK-011`. `REF` = `T/scratch/reviewer2/ws` — исправленная копия референса планировщика (`T/scratch/planner/ws`), собранная и проверенная. Этот план указывает на `REF`, а не на референс планировщика: тот содержит 7 дефектов из раздела 5.

## 1. Summary

Хаб получает слотовый реестр `hub/registry.rs` (чистая логика: слот = `(host, folder_key(cwd), ordinal)`, выбор слота, вложенность, заголовки, иконки, дифф желаемого и применённого состояния темы, `registry.json` через temp + fsync + rename) и актор `hub/slots.rs`, единственного владельца реестра. Актор читает ingress агентов и хуков, служебные `forum_topic_edited` и ответы Telegram. Сам он ничего не ждёт: задания уходят по unbounded-каналу в отдельную dispatch-задачу, и уже она ждёт место в очереди `Scheduler`. На каждый слот в полёте не больше одного вызова темы. Разделитель сессии хранится в реестре, пока Telegram его не принял. Тему получает только сессия, для которой пришёл SessionStart без claude-предка. Неизвестные агенты и prompt-хуки, а также предок, который оказался самой сессией, темы не создают. Иконки берутся только из успешного ответа `getForumTopicIconStickers`: ошибка запроса останавливает старт хаба, недоступные предпочтительные id заменяются id из полученного набора. `/brief` и `/full` в теме слота читают текущую сессию слота через `SlotLocator`. Реализация лежит готовой в `REF`: исполнитель применяет один патч (6 файлов), сверяет sha256 и прогоняет проверки. Новых зависимостей нет.

## 2. Implementation steps

### Step 0. Изоляция

1. `git -C C:/Users/user/dev/cctg status --short -- Cargo.toml Cargo.lock crates` должен быть пуст, ветка `feature/hub-slot-registry`. Если это не так, остановиться и сообщить, ничего не затирать.
2. `.env` не открывать, Telegram не вызывать, `~/.claude` не трогать. Для всей задачи хватает фейкового `Transport`.
3. Одновременно работает один cargo, target вне репозитория:
   ```powershell
   $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task011-impl-target'
   ```

### Step 1. Перенести 6 файлов из `REF`

Основной способ, из корня репозитория (Git Bash):

```bash
git apply --check maw/tasks/in_progress/TASK-011/scratch/reviewer2/task011_final.patch
git apply maw/tasks/in_progress/TASK-011/scratch/reviewer2/task011_final.patch
bash maw/tasks/in_progress/TASK-011/scratch/reviewer2/verify_hashes.sh
```

Запасной способ: скопировать `REF/<path>` в `<path>` байт в байт для каждой строки таблицы и запустить тот же `verify_hashes.sh` (он убирает `\r`, поэтому `core.autocrlf=true` не мешает). Все 6 строк должны дать `OK`. Руками код не править: каждое отклонение от хэша означает, что переносится непроверенный код.

| Path | Изменение | SHA-256 (LF) |
|---|---|---|
| `crates/cctg/src/hub/registry.rs` | новый файл, +1750 | `1012de9db88edaba8dd031b5977d4f736483faac8311210be2cf618b4a14b41d` |
| `crates/cctg/src/hub/slots.rs` | новый файл, +1194 | `de6867c51e19bc1aafccabc940efd7f9a464f2988cd8fd6ac0ea6b67bc499312` |
| `crates/cctg/src/hub/sessions.rs` | +73/−3: `SlotLocator`, `LocateError::NoTranscript` | `43cca08404d49f0cdd62b874e5060ec46cb790e7fb86f8f6cb8cc49cc63d6607` |
| `crates/cctg/src/hub/commands.rs` | +3: ветка `NoTranscript` в `locate_notice` | `ca1631b0ce2a6276736a5a08f4f8f50ff1c57975902c21b5186d8794bae03126` |
| `crates/cctg/src/hub/mod.rs` | +79/−30: модули, загрузка реестра, строгие иконки, `Slots` вместо `drain_ingress`, `Control` из poll | `3efbd68d771763bb3591d9b3c13ad25eec5ae7c5f665064a84b64855bd84253e` |
| `crates/cctg/tests/slots_logs.rs` | новый файл, +173 | `d09c980701044115a2b94032e84566e0e5bf9750e05c5c10f3cbe9a0cdccfdce` |

`Cargo.toml`, `Cargo.lock`, `scheduler.rs`, `wire.rs`, `ingress.rs`, `updates.rs` не меняются.

Что именно делает код (для ревью; в `REF` всё уже есть):

**`hub/registry.rs`**
- `folder_key(cwd)`: снимает `\\?\` (а `\\?\UNC\` превращает в `\\`), меняет `\` на `/`, убирает хвостовые `/`. Для путей с буквой диска и UNC делает `to_lowercase`, POSIX-пути не трогает. `folder_name(cwd)` возвращает последний компонент в исходном написании, он идёт в заголовок.
- `topic_title(host, folder, ordinal, label)`: `[host] folder #N · label`, где `#N` только при ordinal > 1, host не длиннее 32. Длина считается `transcript::telegram_len` и не превышает 128 UTF-16 единиц. Label это ai-title или 8 символов id. При нехватке места первым режется label, за ним folder; `[host]` и `#N` остаются.
- Слот для top-level сессии выбирается по порядку: слот, который она уже держит; её свободный прежний слот (resume); слот предыдущей сессии того же `host/claude_pid` в той же папке (`/clear`, прежняя сессия помечается `ended`); первый свободный ordinal; `max + 1`.
- Вложенность: `parent_claude_pid = None` → TopLevel. Pid есть в `pids` → `Nested { parent: Some }` и слот родителя. Pid неизвестен → `Nested { parent: None }` без слота (NestedUnknownParent). Если pid указывает на саму сессию, `session_started` сразу возвращает `SlotOrParent::Parent(None)` и не меняет существующую запись сессии: kind, slot, claude_pid, pids и ended остаются прежними.
- `apply_hook`: `UserPromptSubmit`/`Stop`/`SessionEnd` неизвестной сессии ничего не создают. Метода `adopt` нет.
- Субагенты с непустым `agent_type` записываются как `agent_id -> { parent_session, slot родителя }`. Пустой тип отбрасывается.
- `topic_work(icons, edits)` отдаёт не больше одного задания на слот и ставит `busy`. Приоритет: `Create` (темы нет) → `Separator` (есть `pending_separator`; поле клонируется, а не забирается `take`) → `Edit` (только если `edits`, только изменившиеся поля).
- Результаты вызовов. `topic_created`. `topic_edited` засчитывается и для `TOPIC_NOT_MODIFIED`. `topic_separated(slot, thread, text)` очищает разделитель, только если он совпадает с `text`. `topic_invalid(slot, thread)` забывает тему, только если слот всё ещё привязан к этому `thread`. `topic_edited`/`topic_separated`/`topic_invalid` для чужого `thread_id` ничего не меняют, `busy` тоже. `topic_failed` запоминает желаемые имя и иконку и не повторяет вызов до `retry_failed`; то же действует для неудачного разделителя. `release` вызывается, когда планировщик остановился: разделитель остаётся в `pending`.
- `Icons::from_offered(ids)`: для каждого состояния берётся предпочтительный id, если Telegram его предлагает; иначе наименьший предложенный id, который не является предпочтительным ни для одного состояния. Меньше четырёх пригодных id даёт `IconError::TooFew`.
- `RegistryStore`: `load` (нет файла → пустой реестр; ошибка разбора, чужая версия или ссылка на несуществующий слот → ошибка, в тексте которой нет содержимого файла), `encode`, `save` (`registry.json.tmp` + `sync_all` + `rename`). После загрузки `after_restart` сбрасывает агентов, `waiting`, `busy`, `failed`.

**`hub/slots.rs`**
- В `Options` остаются `icons`, `can_delete`, `grace` (45 s: правки иконок и имён ждут переподключения агентов, создание тем и разделители идут сразу) и `retry_every` (60 s). Поля `hook_wait` нет.
- `Slots::new` запускает `save_loop` (пишет последний снимок, 3 попытки) и `dispatch_loop`. `dispatch_loop` по порядку делает `Outbox::submit(op).await`, а ответы через `Done` возвращает актору.
- В `run` нет `await`, кроме `select!` по входам. `pump`, `on_control` и `hand_off` синхронные.
- Агент неизвестной сессии лежит в `pending: session -> conn` без срока. Его привязывает первый же хук, после которого сессия известна; при отключении агента запись удаляется.
- `on_topic_done`. Для Separator: успех → `topic_separated`, gone → `topic_invalid`, прочее → `warn` и `topic_failed`. Для Edit: `TOPIC_NOT_MODIFIED` → `topic_edited`. gone (`topic_id_invalid`, `topic_deleted`, `message thread not found`) → `topic_invalid`.
- `read_title` читает транскрипт построчно (`BufReader::read_until`) не дальше 256 MiB и возвращает первый ai-title (`transcript::ai_title` на каждой строке). Чтение идёт через `spawn_blocking`.
- `forum_topic_edited` в теме известного слота удаляется через `Op::Delete`, если у бота есть право. Первая ошибка удаления даёт один `warn`, следующие только `debug`.

**`hub/mod.rs`**
- Объявлены `pub mod registry; pub mod slots;`.
- `RegistryStore::open` и `load` выполняются сразу после `OffsetStore`, до любого обращения к Telegram. Битый файл останавливает старт.
- `can_delete = status == "creator" || can_delete_messages`, при отсутствии права одно предупреждение на старте (уже было).
- `checked_icons(api.get_forum_topic_icon_stickers().await)?`: ошибка запроса или `TooFew` дают ошибку старта. В ней нет токена: `ApiError` проходит через `without_url`. Каждая замена иконки логируется `warn` с именем состояния.
- `Slots::new` и `slots.run` заменяют `drain_ingress`, который удалён вместе с неиспользуемыми импортами.
- `route_inbound(commands, control)` отправляет `Control::TopicEdited`.

**`hub/sessions.rs` / `commands.rs`**
- `SlotLocator`: команда в теме слота без префикса отдаёт текущую сессию слота (`TopicView` из `watch`). General, префикс и незнакомая тема уходят в `ProjectsDir`, как раньше.
- Пустой путь транскрипта даёт `LocateError::NoTranscript`, пользователю показывается текст «У сессии этой темы пока нет известного транскрипта.»

### Step 2. Проверка

По одной команде, target вне репозитория:

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task011-impl-target'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Ожидается: fmt и clippy без замечаний, **231 passed, 0 failed, 1 ignored** (ignored — старый изолированный config-тест). На том же патче в чистом клоне HEAD получено ровно это, лог в `T/scratch/reviewer2/clone_test.txt`. Другое число passed само по себе не провал, но его надо объяснить. Провал — это любой failed или новый ignored.

Проверка на флейки, cargo по одному:

```powershell
1..5 | % { cargo test -p cctg --offline --lib -- hub::slots hub::registry hub::sessions hub::tests }
1..5 | % { cargo test -p cctg --offline --test slots_logs }
```

Ожидается 49 passed в каждом прогоне lib и 1 passed в каждом прогоне slots_logs (эталон в `T/scratch/reviewer2/flake.out.txt`). Прогон lib идёт около 6.5 s, из них большая часть уходит на `a_stalled_telegram_never_stalls_ingress`.

После этого `git diff --check` и `git status --short`: изменены или добавлены ровно 6 путей из таблицы.

### Step 3. Коммит

Английский, без trailer-ов `Generated with` / `Co-Authored-By`:
`feat(hub): durable slot registry and topic lifecycle (TASK-011)`.

## 3. Test plan

| Критерий | Тесты | Что проверяется |
|---|---|---|
| 1. Сессия после смерти прежней: 0 новых тем, 1 разделитель | `registry::a_session_after_a_dead_one_reuses_the_slot_with_one_separator`, `registry::two_starts_after_a_death_take_the_old_slot_and_one_new_ordinal`, `slots::one_slot_lives_through_hook_agent_end_and_the_next_session` | другое написание папки даёт ту же тему. Одна `Send` `── session bbbbbbbb · new ──` в thread 100. B и C, стартовавшие до первого вызова, дают: B в старом слоте без Create, C ровно один Create `#2`. Повторный проход ничего не добавляет |
| 2. Параллельные `#2`, `#3` | `registry::concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first`, `slots::two_live_sessions_make_two_topics_and_waiting_shows` | имена `[box] Project`, `#2`, `#3`. Освободившийся `#2` занимается раньше, чем появится `#4`. Другой host или другая папка дают другой слот |
| 3. Nested/subagent: 0 тем, ссылка на родителя | `registry::nested_runs_and_subagents_get_no_slot`, `registry::a_parent_pid_that_is_the_session_itself_is_nested_unknown_parent`, `slots::nested_and_subagent_events_make_no_topics`, `slots::unknown_sessions_make_no_topics_until_their_session_start`, `registry::prompt_hooks_of_an_unknown_session_create_nothing` | вложенный запуск и вложенный во вложенный указывают на слот родителя. Неизвестный предок: nested без слота. Предок, равный самой сессии: `Parent(None)`, запись и слот A не изменились, после обычного resume A возвращается в свой слот. Stop, UserPromptSubmit и агент без SessionStart дают 0 операций |
| 4. SessionEnd не закрывает тему; иконки из списка | `registry::states_map_to_icons_from_the_list`, `registry::icons_come_from_the_offered_set`, `hub::tests::icons_come_only_from_a_successful_lookup`, `slots::one_slot_...` | после SessionEnd ровно один `EditTopic { name: None, icon: DEAD }`, операции закрытия нет в `Op` вообще. Недоступный предпочтительный id заменяется наименьшим запасным, все 4 id разные и входят в набор. Меньше 4 id или ошибка запроса дают ошибку старта |
| 5. Заголовок ≤128, `[host]` и `#N`, id заменяется на ai-title | `registry::titles_fit_and_keep_host_and_ordinal`, `registry::short_id_becomes_the_ai_title`, `slots::the_ai_title_replaces_the_short_id`, `slots::the_ai_title_is_found_past_the_head_of_a_long_transcript` | кириллица, эмодзи, длинный host, ordinal до `u32::MAX`. ai-title после 5 MiB заполнителя находится, за лимитом не читается |
| 6. Прерванное сохранение; `TOPIC_ID_INVALID` даёт одну замену | `registry::saves_are_atomic_and_round_trip`, `registry::a_broken_or_foreign_file_refuses_to_load_without_quoting_it`, `registry::a_gone_topic_is_replaced_exactly_once`, `registry::a_late_gone_report_during_a_replacement_changes_nothing`, `slots::a_deleted_topic_is_replaced_once`, `slots::a_gone_topic_during_a_session_change_is_replaced_once`, `slots::topic_not_modified_counts_as_applied` | недописанный temp не мешает загрузке. Поздние invalid, edited и separated по старой теме во время замены не снимают `busy`. Итог ровно 2 Create (исходная тема и одна замена), после замены нет вызовов в thread 100 |
| 7. `forum_topic_edited` удаляется; без права один лог | `slots::one_slot_...`, `slots::without_the_delete_right_nothing_is_deleted`, `tests/slots_logs.rs` | `Delete{55}` и больше ничего. Без права ни одного `Delete`. При отказах ровно одна строка `cannot delete a forum service message`. В логах нет папки, пользователя, заголовка и пути |
| 8. Сессия только от хука = «нет канала», агент позже, без второй темы | `registry::a_hook_only_session_is_bound_by_its_agent_later`, `slots::an_agent_before_its_hook_is_bound_without_a_second_topic`, `slots::one_slot_...` | тема создаётся с 👀 и правится на ⚡️. Агент, пришедший раньше хука, ждёт сколько угодно и не создаёт темы, после SessionStart появляется ровно одна тема с ⚡️ |
| 9. `folder_key` | `registry::folder_key_normalizes_windows_spellings_only`, `registry::folder_name_keeps_the_original_spelling`, `registry::folder_spellings_share_one_slot` | три написания из критерия дают один ключ и один слот. UNC. POSIX чувствителен к регистру |
| 10. Existing tests pass | `cargo test --workspace --offline` | 231 passed, 1 ignored |
| Урок TASK-010 QA (ingress не ждёт Telegram) | `slots::a_stalled_telegram_never_stalls_ingress` | транспорт не отвечает никогда. 1100 SessionStart (больше, чем очередь планировщика на 1024 плюс канал хуков на 16) принимаются за 10 s, и все 1100 слотов попадают в `registry.json` |
| Разделитель не теряется | `registry::a_separator_stays_pending_until_it_is_delivered`, `slots::a_failed_separator_is_sent_again` | пока разделитель в полёте, он есть в сохраняемом состоянии. После отказа повтор идёт после `retry_failed`, после остановки планировщика тоже. Правка ждёт разделитель. Отказ, потом успех: ровно две `Send` с одним текстом |

Доказательства в `T/scratch/reviewer2/`:
- `repro.out.txt`: 4 новых slots-теста на **неисправленном** коде планировщика, все 4 FAILED. Это двойная замена темы, остановка ingress, потерянный разделитель и тема для неизвестной сессии.
- `mutations.out.txt`: 23 мутации, 22 KILLED. Выжила M5, «разделитель и для первой сессии нового слота». Она эквивалентна: `occupy` с `current_session = None` бывает только у только что созданного слота без темы, первым для него идёт `Create`, а `topic_created` очищает разделитель.
- `flake.out.txt`: 5 + 5 прогонов без падений.
- `clone_test.txt`: полный прогон на патче в чистом клоне.
- `disconfirmation.md`.
- Скрипты `build_patch.sh`, `verify_hashes.sh`, `mutate.py`, `fix_*.py` воспроизводят всё выше.

Для исправления 1 (ingress) нет однострочной мутации: откат означает вернуть `await` в актор. Роль мутанта сыграл исходный код планировщика: на нём тест падает (`repro.out.txt`), на исправленном проходит.

## 4. Rollout notes

- Миграций нет: `registry.json` новый файл в `<CCTG_STATE_DIR|.cctg>`, `VERSION = 1`. Путь уже в `.gitignore` (`.cctg/`, `registry.json`). Битый файл или файл другой версии останавливает хаб с текстом без содержимого: файл надо исправить или убрать руками. Молча стартовать с пустым реестром нельзя, иначе у каждой папки появится вторая тема.
- Новых переменных окружения и зависимостей нет.
- Новое условие старта: `getForumTopicIconStickers` должен ответить успешно и дать не меньше 4 id. Временная ошибка Telegram на старте теперь останавливает хаб, это цена гарантии, что непроверенный id не уйдёт в Telegram. Перезапуск лечит.
- Поведение для TASK-012/013/018. Сессия, SessionStart которой хаб не видел (хаб был выключен при старте сессии), темы не получает. Она появится на следующем SessionStart этой сессии (`resume`, `clear`, `compact`), а до этого её агент ждёт в `pending`. Для хука TASK-012 контракт такой: `parent_claude_pid = Some(pid)` хаб всегда считает вложенностью (неизвестный pid означает NestedUnknownParent, темы нет). Поэтому `Some` присылается только для реально найденного claude-предка, иначе `None`. `claude_pid` нужен для правила `/clear` и для поиска родителя вложенными запусками.
- Известные ограничения, решения по ним вне этой задачи:
  - Потерянный SessionEnd держит слот в «нет канала», пока не придёт следующий SessionStart в том же процессе; протухание относится к TASK-017.
  - Реестр ключует сессии по id. Вложенный `claude -p --resume <id родителя>` делит запись с родителем, и его SessionEnd пометит родителя `ended`. Тема при этом не создаётся, но иконка станет «мертва» до следующего события родителя.
  - Разделитель при потерянном ответе Telegram (сетевой таймаут после фактической доставки) может прийти дважды. Потеряться он не может. У `sendMessage` нет ключа идемпотентности.
  - Удаление `forum_topic_edited` не связано с конкретным вызовом, поэтому ручное переименование темы пользователем тоже удалит служебное сообщение.
  - Лимит заголовка 128 UTF-16 единиц. Живой замер оркестратора показал, что сервер принимает больше (OPEN_DECISIONS), так что лимит консервативный.
- Сохранение идёт снимком после каждого изменения, промежуточные снимки схлопываются. При убийстве процесса теряется только последнее изменение, атомарность файла сохраняется. Надёжность каталога при потере питания (fsync директории) вне критерия.
- `registry.json` содержит имена хостов, пути транскриптов и заголовки. В логи и тексты ошибок ничего из него не попадает, это проверяет `slots_logs`.

## 5. Review notes

Что изменено относительно PLAN_V2 и референса планировщика, и почему. Каждый дефект сначала воспроизведён тестом.

1. **Ingress зависел от Telegram** (PLAN_V2 п.1, подтверждено). `pump` делал `outbox.submit(op).await` в акторе, а очередь `Outbox` ограничена 1024; пока `transport.execute` висит, она не вычитывается. Воспроизведено: `a_stalled_telegram_never_stalls_ingress` на старом коде падает по таймауту. Исправлено через unbounded dispatch-канал и `dispatch_loop`; `pump` и `on_control` стали синхронными. `scheduler.rs` не трогался: менять ёмкость очереди TASK-008 или выбрасывать задания значило бы терять желаемое состояние. Рост dispatch-очереди ограничен одним заданием на слот плюс удалениями служебных сообщений.
2. **Неизвестные сессии становились top-level** (PLAN_V2 п.2, подтверждено). Воспроизведено: `unknown_sessions_make_no_topics_until_their_session_start`, на старом коде Create и Edit. Удалены `Registry::adopt`, `Options::hook_wait` и дедлайны `Pending`; `Stop`/`UserPromptSubmit` неизвестной сессии игнорируются. Правило одно: без SessionStart темы нет.
3. **Предок, равный самой сессии** (PLAN_V2 п.3, подтверждено, и найдена дыра в самом PLAN_V2). У планировщика это давало TopLevel. PLAN_V2 предлагал «NestedUnknownParent без слота», но при буквальной реализации (перезапись `entry.kind`/`entry.slot`) известная top-level сессия теряет слот навсегда: при следующем обычном resume срабатывает ветка `known session keeps its kind`. Это и был контрпример disconfirmation, он подтвердился на коде (`registry.rs` планировщика, строки 507-512 и 540-541). Исправлено так: ранний возврат `Parent(None)`, запись не меняется. Тест `a_parent_pid_that_is_the_session_itself_is_nested_unknown_parent`, мутации N3a и N3b.
4. **Двойная замена темы** (PLAN_V2 п.4, подтверждено). Воспроизведено через `/clear`, где одно событие даёт разделитель и правку заголовка: на старом коде 3 Create в 3 из 3 прогонов. Первая попытка воспроизведения через SessionEnd + SessionStart прошла на багованном коде, потому что правка иконки съела ошибку; это записано в `log.jsonl` как dead_end. Исправлено: один вызов на слот в полёте, разделитель тоже ставит `busy`, чужой `thread_id` в результате ничего не меняет.
5. **Разделитель терялся** (PLAN_V2 п.5, подтверждено). Воспроизведено: `a_failed_separator_is_sent_again`, на старом коде одна Send и никакого повтора. Исправлено: `clone` вместо `take`, `topic_separated` сверяет текст, отказ идёт через `topic_failed` и повторяется на тике, `release` разделитель не трогает.
6. **Иконки** (PLAN_V2 п.6, подтверждено). Раньше `keep_valid` и умолчания при ошибке. Теперь `Icons::from_offered` и `checked_icons(Result) -> anyhow::Result<Icons>` в `mod.rs`, чистая функция с тестом. `Option<String>` в `Icons` оставлен: `for_state` и `topic_work` его уже используют, менять тип ради гарантии, которую даёт конструктор, значило бы трогать лишние места.
7. **ai-title только в первых 4 MiB** (PLAN_V2 п.7, подтверждено). Теперь потоковое чтение до EOF с лимитом 256 MiB. Тест с заголовком после 5 MiB, мутация N8.
8. Регрессионный тест на «после смерти A одновременно B и C», о котором писал reviewer-1, добавлен: `two_starts_after_a_death_take_the_old_slot_and_one_new_ordinal`. Гонки там нет, так и было установлено.
9. Числа. У планировщика было 221 passed и 12 мутаций. Теперь 231 passed (+4 registry, +5 slots, +1 mod), 23 мутации: 12 мутаций планировщика перенацелены на новый код (M3, M9, M11 по новым образцам) и добавлено 11 новых.
10. Не изменялось, проверено по коду: `folder_key` и заголовки, `/clear` и resume, grace, атомарное сохранение, `SlotLocator`, удаление `forum_topic_edited` и warn-once, отсутствие closeForumTopic.
11. `PCTX_PROPOSALS.md` дополнен записью reviewer-2: в записи планировщика утверждалось, что «хаб принимает сессию, известную только от агента», это больше не так.

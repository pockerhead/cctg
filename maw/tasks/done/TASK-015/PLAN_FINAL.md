# TASK-015 PLAN FINAL: субагенты и вложенные запуски внутри слота родителя

Эталон: `maw/tasks/in_progress/TASK-015/scratch/reviewer2/` (дальше `R2/`). Это копия planner-эталона с исправлениями reviewer-2, собранная и прогнанная целиком. Implementer применяет patch, сверяет hashes и прогоняет проверки. Писать код руками не нужно; каждое изменение ниже объяснено, чтобы его можно было проверить глазами.

- `R2/task015.patch`: diff от текущего HEAD (коммиты после `89c7923` трогают только `maw/`), 6 файлов. `git apply --check` на HEAD проходит.
- `R2/hashes.txt` + `R2/verify_hashes.sh`: sha256 шести файлов (LF-байты, CR вырезается при сверке).
- `R2/ws/`: применённое дерево. `R2/build_patch.py` пересобирает patch и hashes из `ws/`.
- Доказательства: `R2/repro_slots.out.txt` (новые slot-тесты на коде планировщика: 5 FAILED; шестой, `a_first_send_with_an_unclear_answer_is_not_sent_again`, падал отдельно после увеличения ожидания сверх 1 с `min_gap` scheduler-а: два одинаковых send), `R2/mutations.out.txt` (18 мутаций, все KILLED), `R2/workspace_test.txt` (полный прогон). Скрипты правок: `R2/fix_registry.py`, `R2/fix_slots.py`, `R2/fix_tests.py`, `R2/add_tests.py` + `R2/r2tests.rs`.

Ожидаемые sha256 (LF):

```
b4c7f66ab83a3e8517b2dbf4b39578b468df0c21dd87aa6646bb42631cfc638a  crates/cctg/src/hub/mod.rs
1889ab553e4d4227d0d5e4a4f006eae48adbc511f750228512251107881efb88  crates/cctg/src/hub/registry.rs
1713d9ce6346de884cc125fc61f02646950c01b88c591ad50619cd63caf64e1a  crates/cctg/src/hub/slots.rs
d189da2165a85c6f3bebb70eb9d816132f27087f570ac5891ce22fca9401cb6e  crates/cctg/src/hub/subagents.rs
db477600f37e472814ac37428dde45f85c3630d23eec6bfcb030342e0ed57ac9  crates/transcript/src/subagent.rs
a32e60ad384911498bb985ca2204feb595d39da551b074a8a8a27c8a86b4e2d1  crates/transcript/tests/subagent.rs
```

Проверено: patch применён к чистому `git archive HEAD` (HEAD `c94b6ae`), все шесть hashes `OK`.

## 1. Summary

Typed `SubagentStart`/`SubagentStop` родителя верхнего уровня становится кандидатом в памяти slots-актора. В registry и в Telegram он попадает только когда транскрипт родителя показывает вызов `Agent` и его результат с тем же `toolUseResult.agentId`: инкрементальное чтение вне актора, повторные проверки через 1, 2, 4, 8, 16 с в окне 60 с, stop открывает окно заново. Скоррелированный субагент получает один блок `↳ <type> <id>: <description>` + `в работе…` в теме слота родителя; на stop блок редактируется в `Subagent::render()` (отчёт handback > законченный brief по `agent-<id>.jsonl` > `last_assistant_message`), длинный текст режется до 4096 и уходит целиком файлом. Вложенный `claude -p` с известным родителем получает один блок `⇣ nested <id>`, на своём SessionEnd показывает последний ответ (он хранится в registry) или `· завершён`; темы, слота и маршрута канала у него нет. Reply на блок субагента живой текущей сессии слота уходит в канал родителя с meta `target_agent=<agent_id>`. Состояние блоков хранится в `registry.json`; первый send блока at-most-once: исход, который не доказывает отказ Telegram, превращается в tombstone в registry и не повторяется ни после рестарта, ни в том же процессе. Все новые очереди и карты ограничены константами.

## 2. Implementation steps

Все пути от корня репозитория. Порядок выполнения:

0. Проверить, что дерево чистое и HEAD содержит `08ba8ad` (TASK-022): `git status --short` пусто.
1. `git apply maw/tasks/in_progress/TASK-015/scratch/reviewer2/task015.patch`, затем `bash maw/tasks/in_progress/TASK-015/scratch/reviewer2/verify_hashes.sh`: шесть строк `OK`, код выхода 0. При `MISMATCH` остановиться: patch лёг не на тот HEAD.
2. Дальше шаги 3-8 описывают содержимое patch по файлам (для ревью, не для ручного набора).

3. **`crates/transcript/src/subagent.rs`** (как у планировщика).
   - `SubagentInput.description: Option<&'a str>` после `agent_type`: описание из родительского вызова `Agent`, используется, когда в meta нет `description`.
   - `Subagent::new`: description = meta, иначе `input.description`, через `one_line` (режет до 120 символов), пустое отбрасывается.
   - `pub fn header(&self) -> String` (первая строка `render`), `render` использует его. Зачем: заголовок running-блока равен первой строке финального текста без второй копии `agent_header` в hub.
   - `crates/transcript/tests/subagent.rs`: `explore()` получает `description: Some("call description")`; новый тест `the_call_description_stands_in_for_a_missing_meta`.

4. **`crates/cctg/src/hub/subagents.rs`** (новый, `pub mod subagents;` в `hub/mod.rs`). Как у планировщика (`is_agent_id`, `scan`, `AgentIndex`, `Candidates`, `Reports`, `header`, `BodyInput`, `read_body`, `body_text`, `nested_text`, `fit`), плюс правки reviewer-2:
   - `MAX_INDEX_ENTRIES = 1024`: `AgentIndex` хранит не больше 1024 вызовов и 1024 результатов, вытесняет самые старые (`call_order`/`link_order`, `VecDeque`). Причина: индекс живой сессии рос всё время жизни сессии.
   - `MAX_CALL_FIELD = 256`: `subagent_type` и `description` вызова режутся `registry::cut` при сканировании. Причина: один вызов с огромным `description` держал бы его в памяти индекса; заголовок всё равно показывает 120 символов.
   - Тест `an_index_keeps_the_newest_calls_and_short_fields`.

5. **`crates/cctg/src/hub/registry.rs`**. Как у планировщика (`BLOCK_RUNNING`, `BLOCK_LOST`, `MAX_BLOCK_ATTEMPTS`, `nested_header`, `Block`, `BlockKey`, `BlockJob`, `SessionEntry.block`, `SubagentEntry.block`, `confirm_subagent`, `show_block`, `lose_blocks`, `block_done`, `block_failed`, `subagent_of_message`; `apply_hook` больше ничего не пишет для Subagent*-хуков; `VERSION` остаётся 1), плюс:
   - `MAX_SUBAGENTS = 1024` и `SubagentEntry.seen: u64` (`serde(default)`, из `touch()`). `confirm_subagent` при заполнении удаляет самую старую запись без работы (не running, без pending, не busy); если таких нет, новый субагент отклоняется (`false`, блока нет). Причина: записи живой сессии не имели предела.
   - `Block.answer: Option<String>` (`serde(default, skip_serializing_if = "Option::is_none")`), `set_nested_answer(session, answer) -> bool` (только nested с блоком), `take_nested_answer(session)`. `lose_blocks` у потерянного nested-блока выбрасывает answer. Причина: ответ nested run жил только в памяти и терялся при рестарте между Stop и SessionEnd.
   - `block_work(limit: usize)`: не больше `limit` jobs за вызов, остальное ждёт в registry.
   - `block_send_unclear(key)`: первый send с неясным исходом: `busy=false`, `pending=None`, `running=false`, `sending` остаётся `true`. Ветка `None if block.sending` в `block_work` дальше держит блок в tombstone: никакой новый текст (результат, `итог не получен`) его не отправит.
   - `RegistryStore::load`: перед `after_restart` удаляет записи `subagents` с пустым `block.header`. Это записи TASK-011, которые писались на каждый typed hook (включая внутренних агентов и `--agent`); без удаления следующий typed stop того же id постил блок-призрак.
   - Тесты: `a_legacy_subagent_record_is_dropped_on_load`, `an_unclear_first_send_is_never_repeated`, `block_work_hands_out_at_most_its_limit`, `subagent_records_are_bounded_oldest_settled_first`, `a_nested_answer_is_kept_in_the_registry_until_the_run_ends`; тесты планировщика переведены на `block_work(usize::MAX)`.

6. **`crates/cctg/src/hub/slots.rs`**. Как у планировщика (`Options.correlate_for` 60 с, `recheck_after` 1 с; `Done::{Index, Body, Block}`, `Work::Block`; кандидаты, индексы, reports; `on_subagent`, `check_candidates`, `match_candidates`, `confirm`, `finish_block`, `on_block_done`; `target_agent` внутри существующей ветки explicit reply после `live_agent`; `lose_blocks` для уже завершённых сессий в `Slots::new`; `check_candidates` на тике), плюс:
   - `MAX_BLOCK_JOBS = 16` и поле `block_jobs`: `pump` передаёт `block_work(16 - block_jobs)`, `on_block_done` уменьшает счётчик. Причина: при зависшем Telegram каждый pending-блок сидел job-ом в unbounded dispatch-канале.
   - `MAX_BODY_READS = 2`, поля `bodies_waiting: BTreeMap<String, BodyInput>` и `bodies_reading: HashSet<String>`, функция `start_body_reads`. `read_body` кладёт вход в `bodies_waiting` (новый stop того же агента заменяет ждущий), читается не больше двух агентов сразу и не больше одного чтения на агента. `Done::Body { agent_id, text }` применяется, только если для агента не ждёт более новый stop; иначе результат устаревший и выбрасывается. Причина: два stop одного агента (resume через SendMessage) давали два параллельных чтения до 64 MiB, и старый текст мог лечь поверх нового.
   - `on_block_done`: для `BlockJob::Send` повтор только при `send_refused`: `ApiError::Telegram` с кодом 400-499 или `ApiError::Http`, у которого `is_connect()`. Всё остальное (5xx, `Decode`, `Sent` с `message_id == 0`, timeout, `None`) вызывает `block_send_unclear` и один `warn!` без текста. Edit по-прежнему повторяется на тике до `MAX_BLOCK_ATTEMPTS` (edit идемпотентен). Причина: планировщик повторял любой неудачный send, и сообщение, которое Telegram уже показал, уходило второй раз.
   - Nested Stop: `self.registry.set_nested_answer(session, answer)` вместо карты `nested_answers` (карта и `has_nested_block` удалены); `end_blocks` берёт `take_nested_answer`.
   - `confirm`: если кандидат без stop скоррелировался уже после конца своей сессии, сразу `lose_blocks(&[session])`. Причина: иначе блок оставался `в работе…` навсегда (конец сессии прошёл до появления блока).
   - Тестовый `Fake` получил `unclear_sends` (send доходит, ответ не читается: `ApiError::Decode`).
   - Новые тесты: `a_legacy_subagent_record_never_becomes_a_block`, `a_first_send_cut_off_by_a_restart_stays_unsent`, `a_first_send_with_an_unclear_answer_is_not_sent_again`, `a_refused_first_send_is_tried_again`, `a_nested_answer_survives_a_restart_before_its_end`, `a_huge_call_description_still_fits_one_message`, `a_block_confirmed_after_its_session_ended_is_marked_lost`, `block_messages_in_flight_are_capped`, `a_late_body_read_never_overwrites_a_newer_one`.

7. `crates/cctg/src/hub/mod.rs`: `pub mod subagents;`.

8. Не меняются: `hook.rs`, `wire.rs`, `channel.rs`, `agent.rs`, `docs/hook-settings.json` (фильтр пустого `agent_type`, проверка файлов агента, `SubagentHandback`, meta `target_agent` в wire-тесте и в instructions канала уже есть).

## 3. Test plan

Сборка: один `CARGO_TARGET_DIR` под `%TEMP%` (например `%TEMP%\cctg-t015-impl-target`), `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз, каталог удалить в конце. Telegram API не вызывать, `.env`/`device.env` не читать, интерактивный claude не запускать.

1. `cargo test -j 1 -p transcript --test subagent`: 15 passed.
2. `cargo test -j 1 -p cctg --lib hub::`: 227 passed, 1 ignored (ignored был до задачи).
3. `cargo test -j 1 --workspace --no-fail-fast`: всё ok; cctg lib 311 passed / 1 ignored. Эталонный вывод: `R2/workspace_test.txt`.
4. `cargo fmt --all --check` и `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: чисто.

Что покрыто (acceptance criteria):

| AC | Тесты |
|---|---|
| 1. три явных субагента: одна тема, ровно три блока | `three_explicit_subagents_make_three_blocks_and_internal_agents_none` |
| 2. нет блоков-призраков, включая `--agent` | тот же тест (`my-agent` с файлами без вызова), `a_legacy_subagent_record_never_becomes_a_block`, `a_legacy_subagent_record_is_dropped_on_load`, `nested_runs_and_subagents_get_no_slot` |
| 3. отчёт и весь fallback, включая отстающий файл | `the_block_body_follows_the_fallback_order`, `the_body_is_read_from_the_subagent_files`, S1/S2/S3 в slot-тесте, `body_source_order_is_fixed`, `a_late_body_read_never_overwrites_a_newer_one` |
| 4. nested: ноль новых тем, ровно один `⇣ nested` | `a_nested_run_shows_one_block_and_its_answer_only_there`, `nested_and_subagent_events_make_no_topics`, `a_nested_run_gets_one_block_in_its_parents_topic`, `a_nested_answer_survives_a_restart_before_its_end` |
| 5. reply на блок только в канал родителя с `target_agent` | `a_reply_to_a_subagent_block_goes_to_the_parent_with_its_agent_id`, `a_reply_finds_only_its_own_sessions_subagent_block` |
| 6. рестарт без дублей тем и блоков, детерминированная пометка | `after_a_restart_blocks_are_edited_never_sent_again`, `blocks_survive_a_restart_without_a_second_send`, `a_first_send_cut_off_by_a_restart_stays_unsent`, `a_first_send_with_an_unclear_answer_is_not_sent_again`, `an_unclear_first_send_is_never_repeated`, `a_block_confirmed_after_its_session_ended_is_marked_lost` |
| 7. existing tests | полный workspace-прогон |

Пределы: `block_messages_in_flight_are_capped`, `block_work_hands_out_at_most_its_limit`, `subagent_records_are_bounded_oldest_settled_first`, `an_index_keeps_the_newest_calls_and_short_fields`, `candidates_and_reports_are_bounded`.

Мутации (`R2/mutations.py`, прогон `cargo test -p cctg --lib hub::` на каждую): M1-M8 планировщика и M9-M18 на каждое исправление reviewer-2, все 18 KILLED (`R2/mutations.out.txt`). M11 (применить устаревшее чтение тела) в первом прогоне выжила: итоговый текст совпадал, устаревший текст лишь мелькал. Тест `a_late_body_read_never_overwrites_a_newer_one` теперь проверяет, что старый текст не появляется ни на миг; M11 и M12 перепрогнаны после этого и после правила «отказ только 4xx» (`R2/mutations.rerun.out.txt`).

Известный flaky-тест вне задачи: `hub::slots::tests::one_slot_lives_through_hook_agent_end_and_the_next_session` (в diff не входит) один раз упал в холодном прогоне `hub::` сразу после сборки и прошёл 7 раз подряд после. Если упадёт, перезапустить его отдельно; к TASK-015 не относится.

## 4. Rollout notes

- Миграций нет, `registry.json` остаётся `version: 1`: все новые поля defaulted. При первом старте новой версии записи `subagents` без блока (TASK-011) удаляются из registry; hub их больше не показывает и не роутит. Откат на старый бинарник читает новый файл (лишние поля serde игнорирует), но заново начнёт писать записи на каждый typed hook.
- Env и флагов нет. Константы: `Options.correlate_for` 60 с, `Options.recheck_after` 1 с, `MAX_BLOCK_JOBS` 16, `MAX_BODY_READS` 2, `MAX_INDEX_ENTRIES` 1024, `MAX_SUBAGENTS` 1024, `MAX_CANDIDATES`/`MAX_REPORTS` 256.
- Wire не меняется, агентам обновление не нужно; hooks уже шлют `SubagentStart`/`SubagentStop`/`SubagentHandback` (`docs/hook-settings.json`).
- Принятые ограничения (записать в summary реализации):
  - At-most-once первого send. Tombstone значит, что блока в Telegram может не быть, и пометить его нельзя (у Bot API `sendMessage` нет idempotency key). Остаточная дыра: `sending=true` сохраняется асинхронно (`save_loop`), поэтому падение hub в миллисекунды между передачей job и записью файла может дать один повторный send после рестарта. Тот же класс, что orphan topic из TASK-011.
  - Субагенты сессий на другом устройстве блоков не получают, пока агент не отдаёт файлы (шаг 5 порядка разработки). Nested-блоки работают везде.
  - Родительский транскрипт, отставший больше чем на 60 с от последнего хука субагента, скрывает блок (без призрака). Субагенты nested run блоков не получают (решение orchestrator).
  - Handback-отчёт живёт только в памяти: рестарт между handback и stop даёт fallback на brief/last message. Body-чтение, прерванное рестартом, оставляет блок `в работе…` до конца сессии, потом `итог не получен`.
  - Reply на блок, вытесненный `MAX_SUBAGENTS`, приходит без `target_agent`. При 1024 одновременно работающих блоках новый субагент блока не получает.
  - Каждый блок стоит один metered send (+ unmetered edit) из общих ~20 сообщений в минуту группы.

## 5. Review notes

Опровержение, проверенное первым: "старый `registry.json` (TASK-011) с typed-записью внутреннего агента после обновления даёт блок-призрак". Подтвердилось: `on_subagent` видит запись в `registry.subagents`, считает её скоррелированной и отправляет блок. Тест `a_legacy_subagent_record_never_becomes_a_block` падал на коде планировщика (`R2/repro_slots.out.txt`), после фикса проходит.

Находки reviewer-1, проверены по коду эталона планировщика:

1. Legacy ghost: воспроизведено (выше). Исправлено фильтром в `load`.
2. Неограниченные block jobs: воспроизведено (`block_messages_in_flight_are_capped`: 100 busy-блоков при зависшем Telegram). Исправлено `MAX_BLOCK_JOBS`.
3. Индекс растёт всю жизнь сессии: подтверждено по коду (карты без предела, удаление только для не-live сессии), тест на предел добавлен. Исправлено FIFO-пределом; тот же класс проблемы нашёлся в `registry.subagents` живой сессии, добавлен `MAX_SUBAGENTS`.
4. Гонка body-чтений: воспроизведено (`a_late_body_read_never_overwrites_a_newer_one`: старый stop перетирал новый). Исправлено очередью `bodies_waiting` с одним чтением на агента и двумя всего. Вторая половина находки (поздний дубль stop перетирает отчёт) не воспроизведена: второй stop бывает только после resume, и тогда его ответ новее отчёта прошлого запуска; handback приходит раньше stop, потому что Claude Code ждёт завершения PostToolUse-хука. Оставлено как есть.
5. Running header больше 4096: НЕ воспроизведено. `Subagent::new` пропускает type и description через `one_line` (обрезка до 120 символов), id ограничен 64 символами. Тест `a_huge_call_description_still_fits_one_message` проходит и на коде планировщика; оставлен как регрессия. Код заголовка не менялся; урезано только поле в памяти индекса (п.3).
6. Ответ nested run только в памяти: воспроизведено (`a_nested_answer_survives_a_restart_before_its_end`: после рестарта было `· завершён`). Исправлено `Block.answer`.
7. Restart-тесты только на удобный случай: подтверждено. Добавлены tombstone после рестарта с проверкой файла, неясный send в процессе, миграция legacy-файла, nested answer через рестарт.

Новые находки reviewer-2:

- At-most-once нарушался в процессе: `block_failed` повторял любой неудачный send, включая ответ, который не читается после того, как Telegram сообщение принял. Воспроизведено (`a_first_send_with_an_unclear_answer_is_not_sent_again`: два одинаковых send). Исправлено `send_refused` + `block_send_unclear`. `ApiError::Telegram` с кодом 200 ("ok response without result") и 5xx считаются неясными, отказом считаются только 4xx и `is_connect`.
- Блок, скоррелированный после конца сессии и без stop, навсегда оставался `в работе…`. Воспроизведено (`a_block_confirmed_after_its_session_ended_is_marked_lost`). Исправлено в `confirm`.

Ещё раз пытался сломать (не сломалось):
- Призраки: `SendMessage`-результаты с `toolUseResult.agentId` могли бы перезаписать связь агент -> вызов. В 80 реальных транскриптах этой машины 523 результата с `agentId`, все от `Agent`, повторных связей 0. Кандидаты nested run и `NestedUnknownParent` отбрасываются в `on_subagent` (нет своего слота); handback неизвестного агента лежит в ограниченном `Reports` и блока не создаёт.
- `target_agent`: сессия берётся из `live_agent` (живая текущая top-level сессия слота), `subagent_of_message` сверяет parent, thread и message id; reply на nested-блок, на блок прошлой сессии слота, в другой теме или на tombstone (нет `message_id`) meta не получает; соединение nested run ничего не получает.
- TASK-022: nested Stop проходит через `set_nested_answer` и `on_turn_answer`, последний отсекается `current_slot` (nested не top-level); `SubagentStop` в `on_turn_answer` не попадает (другой вариант enum). Проверено тестами `three_explicit_...` (никакой текст stop не стал отдельным сообщением) и `a_nested_run_shows_one_block_and_its_answer_only_there`.

Отклонено из PLAN_V2 как лишнее: generation-счётчики тел (очередь с заменой даёт то же проще), нормализация заголовка (п.5), persist handback-отчётов (приватный объёмный текст в registry), отдельный key `(session, agent_id)` для reports (agent id уникален, хуки аутентифицированы, воспроизведения нет).

PCTX: предложения планировщика код не меняют и оставлены orchestrator-у в `PCTX_PROPOSALS.md`; reviewer-2 дописал туда уточнение про at-most-once и пределы.

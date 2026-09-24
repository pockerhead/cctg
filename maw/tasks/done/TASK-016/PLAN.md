# TASK-016 PLAN: живой поток хода в тему слота

Эталон: `maw/tasks/in_progress/TASK-016/scratch/planner/` (дальше `P/`). Полный дизайн собран и прогнан в копии workspace `P/ws/` (git archive HEAD `2792661` + правки). Implementer применяет patch, сверяет hashes и прогоняет проверки; руками ничего набирать не нужно, шаги ниже объясняют каждое изменение для ревью.

- `P/task016.patch`: diff от HEAD `2792661`, 23 файла (3136 строк). Проверено: `git apply --check` и `git apply` на чистом `git archive HEAD` (autocrlf=true, как в рабочей копии) проходят, все 23 hashes `OK`.
- `P/hashes.txt` + `P/verify_hashes.sh`: sha256 файлов (LF, CR вырезается при сверке). `P/build_patch.py` пересобирает patch и hashes из `P/ws/`.
- Доказательства: `P/lag_probe.run1.out.txt`, `P/lag_probe.i3.out.txt`, `P/passive_lag.interactive-subagent.out.txt` (замер лага), `P/hook_cost.out.txt` (цена хука), `P/reaction_emoji.out.txt` (список реакций Bot API 10.3), `P/workspace_test.txt`, `P/clippy.out.txt`, `P/fmt.out.txt`, `P/mutations.out.txt` + `P/mutations.rerun.out.txt` (19 мутаций, все KILLED). Скрипты: `P/lag_probe.py`, `P/interactive_lag_probe.py`, `P/passive_lag.py`, `P/hook_cost.py`, `P/mutations.py`, `P/edit_*.py`, `P/*_tests.rs`.

## 1. Understanding (что есть сейчас)

- `crates/cctg/src/wire.rs:114-212`: `Register { session_id, host, cwd, claude_pid, verdict_ack }`, `AgentMsg { Hello, Register, Reply, PermissionRequest, PermissionAck }`, `HubMsg { Registered, Rejected, Inbound, PermissionVerdict }`, `VERSION = 1`. Новые типы допустимы только за capability в `Register` (урок TASK-014).
- `crates/cctg/src/agent.rs:298-446`: `run_stdio` поднимает link (`spawn`) и `serve_channel`; события link идут в `channel::Server::on_link` (`channel.rs:166-207`). Агент не знает `transcript_path`: его знает только hub из хуков (`registry.rs:296-322`, `SessionEntry.transcript_path`). После `/clear` агент живёт со старым `CLAUDE_CODE_SESSION_ID`, hub перепривязывает его по pid (`slots.rs:513-552`).
- `crates/cctg/src/hub/scheduler.rs`: один `Scheduler` на все записи Bot API; `Op::Send{permission}` в metered-очереди, prompt обгоняет чужие темы, но не старшие сообщения своей темы (`next_permission`, 329-348); edits коалесцируются (`enqueue`, 350-377); bucket 5 + 1/4 с, min_gap 1 с (`BucketConfig`, 169-184).
- `crates/cctg/src/hub/slots.rs`: актор-владелец registry. `on_topic_message` (915-974) отдаёт Telegram-сообщение агенту `try_send`-ом с meta `message_id`; `on_turn_answer` (1013-1040) шлёт `last_assistant_message` из `Stop` сразу (TASK-022); `send_text`/`send_messages` (1078-1104, 1483-1498) через `dispatch_loop` (1849-1870) в scheduler; separator сессии живёт в `Slot.pending_separator` до ответа Telegram (`registry.rs:273-275, 616-626`).
- `crates/cctg/src/hub/subagents.rs` + `slots.rs:692-721`: образец инкрементального чтения транскрипта по смещению (только целые строки, `spawn_blocking`); это чтение на стороне hub, для удалённых устройств не работает (risk lesson TASK-015).
- `crates/transcript/src/render.rs`: `user_text` (170-205) классифицирует prompt/service/channel, `tool_line` (312-340) даёт brief-строку вызова, `channel_body` (212-228). Всё приватное. `is_final` (232-238): `end_turn` = ответ, `tool_use` = промежуточный текст.
- Хук `UserPromptSubmit` приносит hub-у только `prompt_id` (`hook.rs:186-188`), текста и Telegram `message_id` в нём нет (premise challenge).

## 2. Замер и выбор источника строк вызовов

Замеры (Claude Code 2.1.281, этот хост):

| прогон | что | лаг появления записи после её `timestamp` |
|---|---|---|
| `claude -p`, 3 Bash-вызова (`P/lag_probe.run1.out.txt`) | главный jsonl | user/assistant 0.10-0.59 с, типично 0.13-0.20 с |
| живой интерактивный процесс, 24 записи субагента (`P/passive_lag.interactive-subagent.out.txt`) | jsonl субагента | median 0.16 с, max 0.28 с |
| интерактивный `claude` в скрытой консоли (`P/lag_probe.i3.out.txt`, правило README: CREATE_NEW_CONSOLE + SW_HIDE, trust-диалог через WriteConsoleInputW, одна папка, свой pid-tree) | главный jsonl | `assistant` (text + tool_use) 4.6-6.6 с на `sleep 3`, 1.7-2.0 с на `echo`; `tool_result` 0.10-0.19 с; финальный `end_turn` 0.15 с |

Вывод: в интерактивном главном транскрипте запись ассистента с вызовом пишется только когда инструмент закончился, вместе с `tool_result` (отсюда 4-7 с из грубого замера пользователя: это время работы инструмента плюс классификатор auto mode). Но строка «завершённый вызов» и так ждёт результат, а результат виден через 0.1-0.2 с после конца инструмента. PostToolUse-хук стреляет в тот же момент.

Цена хука на каждый вызов (`P/hook_cost.out.txt`, dev-сборка, включая старт процесса): 17-19 мс с живым hub, ~530 мс при выключенном hub (POST до таймаута на Windows).

Решение: источник строк вызовов = транскрипт, который читает агент сессии (вариант a). Хуки Pre/PostToolUse не добавляются: они не быстрее для завершённого вызова, стоят процесс на каждый вызов и полсекунды простоя на вызов при лежащем hub. Оговорка: `claude -p` пишет запись ассистента сразу по концу ответа, интерактив по концу инструмента; для строки завершённого вызова разницы нет. Записано в `log.jsonl` (decision). Orchestrator: занести это решение в `OPEN_DECISIONS.md` (задача этого требует, мой scope только PLAN/scratch/log).

## 3. Approach

1. **Кто читает.** Агент сессии (он на устройстве транскрипта) читает файл только по запросу hub: `HubMsg::TranscriptRead { session_id, path, from: Option<u64> }` -> `AgentMsg::TranscriptChunk { session_id, from, to, lines: Vec<StreamLine{end, items}>, missing, more }`. Только целые строки: последняя строка без `\n` не читается и остаётся на следующий запрос (как Fluent Bit tail: pos_file + только завершённые строки + skip long lines, https://docs.fluentbit.io/manual/data-pipeline/inputs/tail). `from: None` = с текущего конца файла. Типы доступны только агенту с `Register.transcript_reads = true`; старые агенты не спрашиваются, `VERSION` остаётся 1. Агент открывает только путь вида `.../projects/<project>/<session_id>.jsonl` (link не должен стать способом читать другие файлы).
2. **Что едет по линку.** Не сырые записи (там мегабайтные `tool_result`), а события `transcript::stream_events(line)` одной строки: `Prompt(text)` (набранный в терминале prompt или slash-команда, как в brief), `Channel{message_id}` (meta `<channel ... message_id="N">` или `queued_command`-вложение с таким тегом), `Note(text)` (текст ассистента с `stop_reason: tool_use`), `Call{id, line}` (brief-строка вызова), `Result{id, error}`. Текст ответа хода (`end_turn`/null) не стримится: его шлёт `Stop` (TASK-022), дубля нет. Telegram-промпты не эхоятся.
3. **Кто владеет смещением.** Hub. `registry.sessions[..].stream = Stream { offset, calls, receipts }` в `registry.json`. `offset` = байты транскрипта, все сообщения которых Telegram уже ответил (не «отдано в очередь»). Рестарт hub перечитывает с него: при крахе повторятся максимум сообщения в полёте (at-least-once, как у Fluent Bit при SIGKILL), дописанное во время простоя не теряется. Новый агент (resume = новый процесс) читает с того же смещения. `calls`: вызовы, прочитанные без результата (id + строка, результат после `offset` снова считается неотправленным). Новая сессия (`startup`/`clear`) стримится с байта 0; любой другой первый старт (resume сессии, которую hub не стримил) с конца файла, история не вываливается.
4. **Что уходит в тему.** `> prompt`, текст до вызова как есть, `• Bash: описание ✓` / `• Edit: файл ✗ первая строка ошибки` одной строкой на каждый вызов при появлении его результата, в порядке результатов. Всё через `Op::Stream` в scheduler. Поток сессии ждёт, пока separator её слота уйдёт (`pending_separator == None`), поэтому separator один и перед строками новой сессии.
5. **Лимит и склейка.** В scheduler (там видна очередь): `Op::Stream{merge: true}` строки вызовов, пока токенов хватает на всё ждущее, уходят по одной; когда ждущих сообщений больше, чем токенов, головная строка забирает следующие строки своей темы до первого другого сообщения этой темы, пока влезает в 4096 UTF-16. Склеенные получают `Outcome::Merged`. Permission prompt обгоняет stream-строки своей темы (обычные сообщения по-прежнему нет).
6. **Порядок с ответом хода.** `Stop` стримящейся сессии не шлёт ответ сразу: hub сразу просит следующий чанк и отпускает ответ после чанка первого запроса, отправленного после `Stop` (максимум `Options.hold_answer` = 1.5 с, или сразу, если агент отвалился). Так последние строки хода идут до ответа.
7. **Реакции.** `setMessageReaction` (Bot API 10.3: `👀` и `✍` без U+FE0F есть в списке `ReactionTypeEmoji`, бот ставит не больше одной реакции; `P/reaction_emoji.out.txt`). После успешного `try_send` в агента: 👀 и `message_id` в `stream.receipts` этой сессии. ✍ только когда в транскрипте этой же сессии появился channel-тег с этим `message_id` и он есть в `receipts`. `UserPromptSubmit` реакции не трогает совсем, поэтому prompt из терминала и любой несопоставленный submit ничего не меняют. Реакция едет по unmetered edit-очереди, коалесцируется по `message_id` (👀 и ✍ подряд = один вызов ✍); ошибка только логируется (warn один раз), маршрутизацию не трогает. Лимита на реакции Telegram не публикует; 429 обрабатывается общей паузой scheduler (все методы считаются в flood control: https://github.com/python-telegram-bot/python-telegram-bot/wiki/Avoiding-flood-limits).

Отвергнуто: hub сам читает jsonl (не работает для второго устройства; решение пользователя); агент стримит сам и хранит смещение (нужен ack-протокол, смещение не переживает новый процесс агента после resume); склейка в slots (не видит bucket); ✍ по `UserPromptSubmit` (premise challenge); сырые записи по линку (размер).

## 4. Steps (содержимое patch по файлам)

0. `git status --short` пусто, HEAD содержит `2792661`.
1. `git apply maw/tasks/in_progress/TASK-016/scratch/planner/task016.patch`, затем `bash maw/tasks/in_progress/TASK-016/scratch/planner/verify_hashes.sh`: 23 строки `OK`, код 0. При `MISMATCH` остановиться.
2. Дальше описание для ревью.

3. **`crates/transcript/src/stream.rs`** (новый) + `lib.rs` (`mod stream; pub use stream::{StreamEvent, stream_events}`) + `render.rs` (`UserText`, `user_text`, `tool_line` стали `pub(crate)`, логика не менялась).
   - `pub enum StreamEvent { Prompt(String), Channel{message_id: i64}, Note(String), Call{id, line}, Result{id, error: Option<String>} }`.
   - `pub fn stream_events(line: &str) -> Vec<StreamEvent>`: одна jsonl-строка через существующий `parse`; sidechain пропускается; meta user-текст даёт только `Channel` (атрибут `message_id` открывающего `<channel ...>`-тега, только цифры, тело сообщения не читается); не-meta user-текст через `user_text` (slash-команды `/name args`, service-записи скрыты); assistant-текст только при `stop_reason == "tool_use"`; `tool_use` -> `Call` с `tool_line(name, input, None, None)`; `tool_result` -> `Result`, ошибка = первая непустая строка без обёртки `<tool_use_error>`, через `one_line` (120 символов). Отдельно `attachment` c `attachment.type == "queued_command"` и channel-тегом в `prompt` -> `Channel` (форма очереди Claude Code для промптов во время хода; для channel не наблюдалась, см. риски).
   - `tests/stream.rs` (3 теста) + фикстура `tests/fixtures/stream.jsonl` (синтетическая, форма реальных записей, id и пути обезличены); `tests/purity.rs` сканирует `stream.rs`.

4. **`crates/cctg/src/wire.rs`**: `Register.transcript_reads: bool` (`serde(default)`); `AgentMsg::TranscriptChunk`; `HubMsg::TranscriptRead`; `StreamLine { end, items }`; `StreamItem { Prompt, Channel, Note, Call, Result, #[serde(other)] Other }` (вид от более нового агента пропускается, чанк не ломается). KINDS дополнены. Тесты: round-trip, `transcript_chunks_stay_readable_across_agent_versions`.

5. **`crates/cctg/src/tail.rs`** (новый, `pub mod tail;` в `lib.rs`): `read_chunk(session_id, path, from) -> AgentMsg` (блокирующий, вызывается в `spawn_blocking`) и `is_transcript_path`. Пределы: 4 MiB файла, 64 строки с событиями, 128 KiB текста на чанк (`more: true`, если осталось), 16 KiB на prompt/note, строка длиннее 64 MiB пропускается целиком. Файл, который стал короче, читается с конца. 4 теста (частичная строка ждёт и читается целиком, `None` = конец файла и строки без событий двигают `to`, чужой путь/нет файла = `missing`, длинный хвост идёт ограниченными чанками по порядку).

6. **`crates/cctg/src/agent.rs`**: `Register.transcript_reads = true`; в `serve_channel` событие `HubMsg::TranscriptRead` не уходит в `channel::Server`, а `spawn_read` читает чанк в `spawn_blocking` и кладёт ответ в outbox. `channel.rs`: `on_link` игнорирует `TranscriptRead` (исчерпывающий match). Тест `a_transcript_read_is_answered_over_the_link_and_never_reaches_claude`.

7. **`crates/cctg/src/hub/api.rs`**: `set_message_reaction(message_id, emoji)`.

8. **`crates/cctg/src/hub/scheduler.rs`**: `Op::Stream { thread_id, text, merge }` (metered, lane Message), `Op::React { message_id, emoji }` (lane Edit, коалесцируется по `message_id`), `Outcome::Merged`, `Job.merged`, `Op::thread`, `merge_lines` (условие склейки `ждущих + 1 > tokens` после refill; только строки `merge: true` той же темы; стоп на первом другом сообщении темы и на 4096 UTF-16); `next_permission` не считает stream-строки старшими сообщениями темы. 5 тестов.

9. **`crates/cctg/src/hub/registry.rs`**: `SessionEntry.stream: Option<Stream>` (`serde(default, skip_serializing_if)`), `Stream { offset: Option<u64>, calls: Vec<PendingCall>, receipts: Vec<i64> }`, `PendingCall { id, line, result_end }`. В `session_started` top-level сессия без `stream` получает `offset = Some(0)` при `startup|clear`, иначе `None`; nested не стримятся. Тест `a_new_transcript_streams_from_its_start_and_a_resume_keeps_its_offset`. `registry.json` остаётся `version: 1`.

10. **`crates/cctg/src/hub/stream.rs`** (новый, `pub mod stream;` в `hub/mod.rs`): чистая логика. `apply(stream, lines) -> Applied { messages: Vec<Out{end, text, merge}>, working }`, `receipt`, `answered_up_to` (двигает offset и забывает вызовы с результатом до него), `Live` (в памяти: `read_at`, запрос в полёте, `next_read`, `held`, очередь `(номер, end, answered)` для «отвеченного префикса»), `Held`. Пределы: `MAX_CALLS` 64, `MAX_RECEIPTS` 32, `MAX_WAITING` 64. 5 тестов.

11. **`crates/cctg/src/hub/slots.rs`**:
    - `Options.stream_every` (300 мс), `Options.hold_answer` (1.5 с); `READ_TIMEOUT` 10 с; `Conn.reads`; `Slots.streams: HashMap<String, Live>`, `reaction_warned`; `Work/Done::Stream{session, number}`, `Work/Done::Reaction`.
    - `stream_target(session)`: живая top-level текущая сессия слота, у слота есть тема и нет `pending_separator`, есть `stream` и `transcript_path`, привязанный агент с `reads`.
    - `pump_streams` (из `pump`): одна просьба в полёте на сессию, раз в `stream_every`, пока неотвеченных < 64; повтор после 10 с без ответа; отпускает `held` по сроку или когда агента нет; `Live` сессии, которая больше не текущая, удаляется после того, как все её сообщения отвечены (иначе resume повторил бы хвост).
    - `on_chunk`: только ответ на свой запрос, `from` должен совпасть с `read_at`; `missing` -> один `warn!` на эпизод, опрос продолжается; иначе `apply`, каждое сообщение через `split_for_telegram` в `Op::Stream` (склейка разрешена только однокусковым строкам вызовов), ✍ для `working`, `read_up_to(to)`, при `more` следующий запрос сразу; затем отпускает `held`, если чанк отвечал на запрос после `Stop`. `registry.dirty` только при непустом чанке или сдвиге offset (холостой опрос не пишет `registry.json`).
    - `on_done` `Stream` -> `answered` + `stream_answered` (двигает persisted offset по отвеченному префиксу).
    - `on_topic_message`: после успешного `try_send` receipt + 👀.
    - `on_turn_answer`: стримящаяся сессия кладёт ответ в `held` (предыдущий held уходит сразу) и просит чанк немедленно; иначе как было.
    - Существующий тест `a_topic_message_reaches_only_the_agent_of_its_slot` теперь ждёт две реакции 👀 вместо «ноль операций». Тестовый `Fake` получил `react_error`. Новые тесты (9): `appended_lines_reach_the_slot_topic_in_order_and_a_partial_line_waits`, `a_restart_neither_repeats_nor_loses_stream_lines`, `a_new_session_in_the_slot_streams_after_its_one_separator`, `eyes_on_hand_off_and_writing_only_for_the_same_messages_channel_record`, `a_refused_reaction_never_stops_routing`, `a_turn_answer_follows_the_lines_read_after_its_stop`, `a_held_answer_goes_out_when_the_agent_never_answers`, `an_agent_without_transcript_reads_is_never_asked`, `idle_reads_do_not_rewrite_the_registry`. Агенты в тестах читают реальный временный jsonl через `crate::tail::read_chunk`.

12. **`crates/cctg/tests/stream_logs.rs`** (новый бинарь, глобальный subscriber): нет файла -> ровно одно предупреждение при ≥10 опросах, файл появился -> строка ушла; путь, имя проекта и текст промпта в логи не попадают. `Register { .. transcript_reads: false }` добавлен в литералы `tests/{ingress,message,permission,slots}_logs.rs` и `hub/ingress.rs`.

13. Не меняются: `hook.rs`, `docs/hook-settings.json` (новых хуков нет), `channel::INSTRUCTIONS`.

## 5. Test plan

Один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз, каталог удалить в конце. Без Telegram, без `.env`/`device.env`, без интерактивного claude.

1. `cargo test -j 1 -p transcript --test stream`: 3 passed.
2. `cargo test -j 1 -p cctg --lib stream`, затем `--lib hub::scheduler`, `--lib tail`: зелёные (фильтрованные прогоны при нехватке памяти).
3. `cargo test -j 1 -p cctg --test stream_logs`: 1 passed.
4. `cargo test -j 1 --workspace --no-fail-fast`: всё ok; cctg lib 341 passed / 1 ignored (на HEAD 315 / 1 ignored; ignored был до задачи). Эталон: `P/workspace_test.txt`.
5. `cargo fmt --all --check`, `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: чисто (`P/fmt.out.txt`, `P/clippy.out.txt`).

| Acceptance criterion | Чем закрыт |
|---|---|
| 1. дописанные строки -> turns в тему нужного слота по порядку | `appended_lines_reach_the_slot_topic_in_order_and_a_partial_line_waits`, `a_call_line_goes_out_with_its_result_in_call_order`, `a_turn_streams_its_prompt_notes_and_calls_but_not_its_final_answer`, `a_transcript_read_is_answered_over_the_link_...` |
| 2. частичная строка не уходит и не теряется | `only_complete_lines_are_read_and_a_partial_one_waits_whole`, тот же slot-тест (строка разрезана посередине), `a_partial_or_foreign_line_gives_nothing` |
| 3. рестарт hub: без повторов, без потерь | `a_restart_neither_repeats_nor_loses_stream_lines` (ждёт сохранённый offset в `registry.json`, пишет строки «в простое», новый актор шлёт только новые), `a_result_read_again_after_a_restart_goes_again_only_if_unanswered`, `the_offset_moves_only_over_answered_messages` |
| 4. планировщик: 20/мин, FIFO в теме, уступает permission | все метерятся как `Send` (`group_limit_and_topic_order_hold` на общий bucket), `stream_lines_held_back_by_the_limit_merge_in_order_without_loss`, `a_permission_prompt_overtakes_the_stream_lines_of_its_topic` |
| 5. ротация: новый поток, новое смещение, separator ровно раз | `a_new_session_in_the_slot_streams_after_its_one_separator`, `a_new_transcript_streams_from_its_start_and_a_resume_keeps_its_offset` |
| 6. нет/удалён файл: одно предупреждение, опрос дальше | `tests/stream_logs.rs`, `a_missing_file_or_a_foreign_path_is_missing` |
| 7. 👀 при передаче, ✍ только по записи канала с этим id; терминал и UserPromptSubmit не трогают; ошибка реакции не ломает маршрутизацию | `eyes_on_hand_off_and_writing_only_for_the_same_messages_channel_record`, `only_a_received_message_turns_to_working_and_only_once`, `a_refused_reaction_never_stops_routing`, `reactions_are_unmetered_and_the_newest_one_per_message_wins` |
| 8. вызов = одно сообщение-строка по порядку, при упоре склейка без потерь | `stream_lines_go_one_per_message_while_the_budget_has_room`, `stream_lines_held_back_by_the_limit_merge_in_order_without_loss`, `a_merged_message_stays_within_the_telegram_limit` |
| 9. замер лага и обоснование источника | раздел 2, `P/lag_probe.*`, `P/hook_cost.out.txt`, decision в `log.jsonl` |
| 10. existing tests | полный workspace-прогон |

Дополнительно: `a_turn_answer_follows_the_lines_read_after_its_stop`, `a_held_answer_goes_out_when_the_agent_never_answers`, `an_agent_without_transcript_reads_is_never_asked`, `idle_reads_do_not_rewrite_the_registry`, `transcript_chunks_stay_readable_across_agent_versions`.

Мутации (`P/mutations.py`, прогон `cargo test -p cctg --lib --test stream_logs` или `-p transcript --test stream`): M1-M19, все KILLED (`P/mutations.out.txt`; M11 в первом прогоне выжила, потому что старый агент отсекался дважды; лишний фильтр убран, M11 и новая M19 перепрогнаны: `P/mutations.rerun.out.txt`).

Живой smoke (для QA, не для implementer): hub + одна интерактивная сессия с каналом по `docs/poc.md`, 2-3 Bash-вызова, сообщение из темы во время хода. Проверить: строки вызовов и 👀/✍ в теме, ответ после строк. Отдельно посмотреть, какой записью Claude Code кладёт channel-сообщение, пришедшее во время хода (`queued_command` или `user`).

## 6. Risk areas

- **Форма channel-сообщения во время хода не проверена.** Наблюдались только сообщения в простаивающую сессию (`queue-operation` enqueue/dequeue + meta `user`). Промпты во время хода Claude Code кладёт как `attachment` `queued_command`; код понимает обе формы. Если channel придёт третьей формой, сообщение останется с 👀 (без ложного ✍). QA smoke это покажет.
- **At-least-once на крахе.** Смещение двигается по ответу Telegram. Если hub упал между приёмом сообщения Telegram и записью `registry.json`, после рестарта повторятся сообщения в полёте (≤64 строк на сессию, обычно одна склейка). Потерь нет. Graceful shutdown у hub нет, так что это и путь Ctrl+C.
- **Сессии, живые в момент обновления.** Их агенты старые (живут до конца сессии) и `stream` в registry у них нет до следующего SessionStart: поток начнётся со следующей сессии/resume. Откат на старый hub: лишнее поле `stream` serde игнорирует.
- **Нагрузка.** Каждая стримящаяся сессия: запрос раз в 300 мс (open + seek + metadata на агенте, две строки по линку). Для второго устройства через Tailscale это ~7 байт/с полезной нагрузки плюс кадры; приемлемо. Если опрос окажется дорогим, поднять `stream_every`.
- **Общий бюджет 20/мин.** Поток ест тот же бюджет, что ответы, reply и notices. Склейка сжимает строки вызовов, но prompt/note идут отдельными сообщениями. Длинный ход = много сообщений; ответы хода встают в ту же FIFO темы.
- **Rolling `calls`.** Прерванный вызов без результата висит в `calls` до вытеснения (64 на сессию) и хранится в `registry.json` у закончившихся сессий. Размер ограничен, но не нулевой.
- **`/clear`.** Агент переезжает на новую сессию по pid (TASK-013), путь нового транскрипта hub берёт из SessionStart; старая сессия дочищается, когда её сообщения отвечены. Покрыто косвенно (ротация через SessionEnd/Start); отдельного теста на `/clear` + поток нет.
- **Реакции.** Если в группе реакции ограничены (`available_reactions`), Telegram отвечает `REACTION_INVALID`; это warn один раз, маршрутизация идёт. Отдельного лимита на `setMessageReaction` не опубликовано; 429 ставит на паузу весь scheduler, как любой метод.
- **Порядок reply vs поток.** `reply`-инструмент приходит мгновенно, строки вызовов позже (конец инструмента + ≤300 мс). Reply может обогнать строку вызова, сделанного до него. Не критерий задачи.

## 7. Open questions

1. **Prompt из терминала в теме** (`> текст`): включено как часть brief-вида хода (описание задачи «turn в brief-виде»). UX-решение пользователя перечисляет вызовы, промежуточный текст и ответ, про промпты молчит. Если пользователь не хочет видеть терминальные промпты, убрать ветку `StreamEvent::Prompt` в `hub/stream.rs::apply` (одна строка, мутация M18 показывает, какие тесты поправить).
2. **Строка `Agent`-вызова** (`↳ Explore: описание ✓`) приходит сразу при запуске субагента (результат `async_launched` мгновенный) рядом с блоком субагента TASK-015. Оставлено по заметке orchestrator; если дублирование мешает, фильтровать `Call` с именем `Agent` в `apply`.
3. **Запись решения в `OPEN_DECISIONS.md`**: делает orchestrator (раздел 2 этого плана + decision в `log.jsonl`).
4. PCTX-предложения: `maw/tasks/in_progress/TASK-016/PCTX_PROPOSALS.md` (лаг записей, форма channel-записей, `setMessageReaction` в списке методов, строка «implemented» для hub/channel).

Уборка после планировщика: временный target и папка пробы `%TEMP%\cctg-t016-probe` удалены. В `~/.claude/projects/` остались транскрипты проб в проекте `C--Users-user-AppData-Local-Temp-cctg-t016-probe` (3 записи, только тестовые команды), а в `~/.claude.json` есть trust-ключ этой папки; удаление по желанию пользователя (шаг пользователя, subagent туда не пишет).

# TASK-016 PLAN_FINAL: живой поток хода в тему слота

Эталон: `maw/tasks/in_progress/TASK-016/scratch/reviewer2/` (дальше `R/`). `R/ws/` это копия workspace планировщика (`scratch/planner/ws/`) с исправлениями этого ревью, собранная и прогнанная целиком. Implementer применяет `R/task016.patch` к текущему HEAD, сверяет hashes и прогоняет проверки. Руками ничего набирать не нужно; раздел 2 объясняет каждое изменение, чтобы ревью и QA знали, что проверять.

## 1. Summary

Агент сессии (`cctg agent`, он всегда на устройстве своего jsonl) по запросу hub читает свой транскрипт с заданного смещения, только целые строки, и отдаёт по agent-линку события строк (`transcript::stream_events`): набранный в терминале prompt, текст до вызова инструмента, вызов, результат, конец хода, запись канала с `message_id`. Hub владеет смещением в `registry.json` и превращает события в сообщения темы слота через планировщик: `> prompt`, текст до вызова, одна строка на завершённый вызов (`• Bash: описание ✓`, `• Edit: файл ✗ ошибка`) строго в порядке вызовов. Строки вызовов при упоре в лимит 20/мин склеиваются в планировщике без потерь. Смещение двигается только через барьер, все сообщения до которого Telegram принял; отказ Telegram останавливает смещение и поток перечитывает хвост (at-least-once, порядок сохраняется). Финальный ответ хода по-прежнему шлёт хук `Stop` (TASK-022), но для стримящейся сессии он ждёт, пока в транскрипте прочитан конец этого хода (не дольше 5 с), поэтому строки вызовов идут до ответа. Сообщение из Telegram, отданное агенту, получает 👀, а ✍ только когда в транскрипте этой сессии появилась запись канала `source="cctg"` с его `message_id`. Источник строк вызовов выбран по замеру: транскрипт, без хуков Pre/PostToolUse (`OPEN_DECISIONS.md`, раздел 4 ниже).

## 2. Implementation steps

### Шаг 0. Проверка исходного состояния

1. `git status --short` пусто, ветка `feature/hub-turn-streaming`. HEAD может быть любым коммитом, в котором `crates/**` совпадают с `e7b16b6` (после него в ветку шли только файлы `maw/`). Проверка: `git diff --stat e7b16b6 HEAD -- crates Cargo.toml Cargo.lock docs` пусто.
2. `git apply --check maw/tasks/in_progress/TASK-016/scratch/reviewer2/task016.patch` проходит.

### Шаг 1. Применить эталон

1. `git apply maw/tasks/in_progress/TASK-016/scratch/reviewer2/task016.patch`.
2. `bash maw/tasks/in_progress/TASK-016/scratch/reviewer2/verify_hashes.sh`: все строки `OK`, код 0 (hashes по LF-байтам, CR вырезается при сверке, рабочая копия с `core.autocrlf=true`). При любом `MISMATCH` остановиться и не править руками.
3. Patch НЕ содержит `scratch/planner/task016.patch`: тот эталон устарел, его hashes не использовать.

Файлы patch (23, все в scope задачи):

| Файл | Что |
|---|---|
| `crates/transcript/src/stream.rs` (новый), `lib.rs`, `render.rs`, `tests/stream.rs`, `tests/fixtures/stream.jsonl`, `tests/purity.rs` | чистое извлечение событий строки |
| `crates/cctg/src/wire.rs` | additive wire-контракт |
| `crates/cctg/src/tail.rs` (новый), `lib.rs` | чтение транскрипта агентом |
| `crates/cctg/src/agent.rs`, `channel.rs` | один читатель на агент, capability в `Register` |
| `crates/cctg/src/hub/api.rs` | `set_message_reaction` |
| `crates/cctg/src/hub/scheduler.rs` | `Op::Stream`, `Op::React`, склейка, `Merged` только после успеха |
| `crates/cctg/src/hub/registry.rs` | `SessionEntry.stream` |
| `crates/cctg/src/hub/stream.rs` (новый), `hub/mod.rs` | редуктор событий, барьеры, удержанные ответы |
| `crates/cctg/src/hub/slots.rs` | интеграция в актор |
| `crates/cctg/src/hub/ingress.rs`, `tests/{ingress,message,permission,slots}_logs.rs` | только `transcript_reads: false` в литералах `Register` |
| `crates/cctg/tests/stream_logs.rs` (новый) | логи: одно предупреждение на эпизод, без путей и текста |

### Шаг 2. Что делает каждое изменение (для ревью)

**2.1 `crates/transcript/src/stream.rs`** (новый; `lib.rs`: `mod stream; pub use stream::{StreamEvent, stream_events};`; `render.rs`: `UserText`, `user_text`, `tool_line` стали `pub(crate)`, логика не менялась; `tests/purity.rs` сканирует новый файл).
- `pub enum StreamEvent { Prompt(String), Channel { message_id: i64 }, Note(String), Call { id, line }, Result { id, error: Option<String> }, TurnEnd }`.
- `pub fn stream_events(line: &str) -> Vec<StreamEvent>`: одна jsonl-строка. Перед разбором `trim()` и снятие UTF-8 BOM (U+FEFF не JSON whitespace; иначе первая строка файла с BOM терялась бы навсегда, offset её уже прошёл). Sidechain пропускается. Meta user-текст даёт только `Channel`, и только если открывающий тег `<channel ...>` имеет `source="cctg"` и числовой `message_id` (тело не читается). Причина: в реальных транскриптах есть другие channel-серверы (`plugin:fakechat:fakechat`, `webhook`) со своими числовыми `message_id`. `attachment` с `attachment.type == "queued_command"` и таким же тегом в `prompt` тоже даёт `Channel`. Не-meta user-текст через `user_text` (slash-команды как `/name args`, service-записи скрыты). Assistant-текст: при `stop_reason == "tool_use"` это `Note`; при любом другом непустом `stop_reason` запись с текстовым блоком даёт `TurnEnd` (текст не передаётся: его шлёт `Stop`). `thinking`-запись того же ответа тоже несёт `end_turn`, но у неё нет текстового блока, поэтому `TurnEnd` один на финальный текст. `tool_use` даёт `Call` с `tool_line(name, input, None, None)`. `tool_result` даёт `Result`, `error` только при `is_error == true`: первая непустая строка без обёртки `<tool_use_error>`, через `one_line`.
- Тесты `tests/stream.rs`: 4 (фикстуры `final_answer.jsonl` и `stream.jsonl`, `TurnEnd` в нужных местах, чужой `source` и queued-запись чужого сервера ничего не дают, строка с BOM и CRLF даёт свой `Prompt`, частичная/чужая строка ничего не даёт).

**2.2 `crates/cctg/src/wire.rs`** (`VERSION` остаётся 1).
- `Register.transcript_reads: bool` (`#[serde(default)]`). Старый агент поле не шлёт и запросов не получает.
- `HubMsg::TranscriptRead { session_id, path, from: Option<u64> }` (`None`: с текущего конца файла). Шлётся только агенту с `transcript_reads`.
- `AgentMsg::TranscriptChunk { session_id, from, to, lines: Vec<StreamLine>, missing, more, reset }`; `lines`, `missing`, `more`, `reset` с `#[serde(default)]`. `reset`: файл больше не продолжается с `from` (короче, или байт `from-1` не перевод строки), читать с 0.
- `StreamLine { end, items: Vec<StreamItem> }`; `StreamItem { Prompt, Channel, Note, Call, Result, TurnEnd, #[serde(other)] Other }` (`tag = "kind"`). Вид от более нового агента пропускается, чанк не ломается.
- `KINDS` дополнены `transcript_chunk` / `transcript_read`. Тесты: round-trip всех вариантов, `transcript_chunks_stay_readable_across_agent_versions`.

**2.3 `crates/cctg/src/tail.rs`** (новый, `pub mod tail;` в `lib.rs`).
- `pub fn projects_root() -> Option<PathBuf>`: `<CLAUDE_CONFIG_DIR>/projects`, иначе `<USERPROFILE|HOME>/.claude/projects` (на Windows сначала `USERPROFILE`). Это единственный каталог, из которого агент читает.
- `pub fn read_chunk(root: Option<&Path>, session_id, path, from) -> AgentMsg` (блокирующая, вызывается из `spawn_blocking`). Файл открывается, только если `session_id` plain (`[A-Za-z0-9-]`, ≤64), канонический путь файла равен `<канонический root>/<одна папка>/<session_id>.jsonl` и это обычный файл. Так закрыты `..`, symlink и junction наружу, чужой файл с похожим путём, файл другой сессии, файл глубже или вне root. Иначе, как и при отсутствии файла, ответ `missing` (без пути и текста ошибки ОС).
- `from > len` или байт `from-1` не `\n` даёт `reset` (закрывает зацикливание после truncate и чтение с середины заменённого файла; offsets hub-а всегда концы строк).
- Только строки с `\n`; последняя незавершённая остаётся на следующее чтение. CRLF и BOM не сдвигают байтовые offsets. Строка длиннее `MAX_RECORD` (64 MiB) пропускается целиком.
- Границы кадра проверяются до добавления строки: не больше 4 MiB просмотра, 64 строк с событиями, 256 items и 128 KiB «веса» (байты текста + 32 на item). Строка, которая не влезает за уже взятыми, остаётся на следующее чтение (`more: true`); одиночная сверхбольшая строка обрезается по items. Prompt/note/строка вызова/ошибка режутся до 16 KiB, id длиннее 256 байт выбрасывает item. Даже при экранировании каждого байта как `\u00XX` кадр меньше `wire::MAX_LINE`.
- Тесты (8): частичная строка ждёт и читается целиком; BOM+CRLF сохраняют offsets; `None` = конец файла, строки без событий двигают `to`; нет файла, чужое место (вне root, глубже, через `..`, другой root, `None` root, чужое имя, не-plain id) = `missing`; junction наружу не читается (Windows, `mklink /J`, если недоступно, тест пишет skip и проходит); укороченный и заменённый файл дают `reset`, с 0 читаются нормально; длинный хвост идёт кусками по порядку; никакая запись не даёт кадр ≥ `MAX_LINE` (control-символы, 7 заметок по 16 KiB в строке, 2000 вызовов в одной записи).

**2.4 `crates/cctg/src/agent.rs`, `channel.rs`.**
- `Register.transcript_reads = true`.
- `serve_channel(frames, output, hub, events, projects: Option<PathBuf>)`: `run_stdio` передаёт `tail::projects_root()`. Один читатель на процесс агента (`spawn_reader`): канал запросов ёмкостью 1, один `spawn_blocking` за раз, ответ в outbox линка. Цикл канала только `try_send`-ит запрос и при занятом слоте его отбрасывает (hub переспросит через 10 с или на новом соединении). Итого не больше одного блокирующего чтения и одного ждущего запроса. `TranscriptRead` никогда не уходит в `channel::Server` (Claude его не видит); `channel.rs` `on_link` игнорирует его в исчерпывающем match.
- Тест `a_transcript_read_is_answered_over_the_link_and_never_reaches_claude` (с root временного каталога).

**2.5 `crates/cctg/src/hub/api.rs`**: `set_message_reaction(message_id, emoji)`, тело `{"chat_id","message_id","reaction":[{"type":"emoji","emoji":...}]}`. `👀` и `✍` (без U+FE0F) есть в списке `ReactionTypeEmoji` Bot API (`scratch/planner/reaction_emoji.out.txt`); бот ставит одну реакцию.

**2.6 `crates/cctg/src/hub/scheduler.rs`.**
- `Op::Stream { thread_id, text, merge }`: metered, lane `Message`, один FIFO с обычными сообщениями. `Op::React { message_id, emoji }`: unmetered lane `Edit`, новая реакция того же сообщения заменяет ждущую (`Superseded`).
- `next_permission` не считает stream-строки старшими сообщениями темы: permission prompt обгоняет их (обычные сообщения своей темы по-прежнему нет).
- `merge_lines`: только для головы `Op::Stream{merge:true}`, только когда ждущих сообщений больше, чем токенов после refill; забирает следующие `merge`-строки той же темы до первого другого сообщения этой темы, пока влезает в 4096 UTF-16 (`transcript::telegram_len`).
- Исправление ревью: склеенные строки получают `Outcome::Merged` только если общий send успешен; при ошибке их receivers закрываются без ответа, и hub считает их недоставленными. 429 по-прежнему возвращает всю склейку в голову очереди.
- Тесты (6 новых): строки по одной при запасе токенов; склейка без потерь и без перехода через обычное сообщение темы; склейка ≤4096; permission обгоняет stream; реакции unmetered и новейшая побеждает; отказ склеенного сообщения не отвечает `Merged` ни одной его строке.

**2.7 `crates/cctg/src/hub/registry.rs`.**
- `SessionEntry.stream: Option<Stream>` (`serde(default, skip_serializing_if)`), `registry.json` остаётся `version: 1`.
- `Stream { offset: Option<u64>, calls: Vec<PendingCall>, receipts: Vec<i64> }`: `offset` = байт транскрипта, до которого все stream-сообщения приняты Telegram; `calls` = вызовы хода, открытые на этом байте (нет результата, или его ждёт более ранний вызов); `receipts` = Telegram-сообщения этой сессии с 👀, ждущие ✍ (≤32, старейшее остаётся с 👀).
- `PendingCall { id, line, done: bool, error: Option<String> }`.
- `session_started`: top-level сессия без `stream` получает `offset = Some(0)` при `source` `startup|clear` (новый файл), иначе `None` (первое чтение с конца файла, история resume не вываливается). Известная сессия сохраняет свой `stream`. Nested не стримятся. Тест `a_new_transcript_streams_from_its_start_and_a_resume_keeps_its_offset`.

**2.8 `crates/cctg/src/hub/stream.rs`** (новый, `pub mod stream;` в `hub/mod.rs`). Чистая логика.
- `apply_line(calls, receipts, items) -> Vec<Step>`, `Step { Send{text, merge}, Working(i64), TurnEnd, NewTurn }`. `Call` добавляет открытый вызов (дубликат id игнорируется). `Result` помечает свой вызов `done` и отпускает только готовые вызовы с головы: `Result(B)` перед `Result(A)` ждёт A (контрпример ревью 1). `Prompt`, `Note`, `TurnEnd` сначала отпускают ход: готовые вызовы в порядке вызовов, вызовы без результата не показываются (нет ложного ✓). 65-й открытый вызов отпускает старейший так же (граница без блокировки: маркер конца хода лежит в файле дальше, остановка чтения дала бы deadlock). `Channel` из `receipts` даёт `Working` один раз.
- `Live` (память): `read_at`, `calls` на `read_at`, запрос в полёте `(conn, when)`, `next_read`, флаги предупреждений, `held: VecDeque<Held>`, `ends_unclaimed`, очередь `waiting` из сообщений (`Waiting|Accepted|Refused`) и барьеров `(to, снимок calls)`. `advance()` снимает принятую голову и возвращает последний пройденный барьер; через `Refused` не проходит. `stuck()`: есть `Refused` и ничего не в полёте. `rewind()`: начать заново с зафиксированного барьера, сохранив удержанные ответы.
- Тесты (6): порядок вызовов при обратном порядке результатов; конец хода отпускает готовые и не метит незавершённые; граница открытых вызовов; ✍ только по своей квитанции и один раз; смещение только через принятые; отказ останавливает смещение до rewind.

**2.9 `crates/cctg/src/hub/slots.rs`.**
- Константы: `READ_TIMEOUT` 10 с, `STREAM_QUEUE = MAX_QUEUED_MESSAGES / 2` (128), `MAX_REACTIONS` 64. `Options`: `stream_every` 300 мс, `hold_answer` 5 с, `stream_retry` 5 с.
- `stream_target(session)`: живая top-level текущая сессия слота, у слота есть тема и нет `pending_separator`, есть `stream` и `transcript_path`, привязанный агент с `reads`. Поток новой сессии слота начинается только после принятого separator: separator ровно один и перед строками.
- `pump_streams` (из `pump`): сессия не текущая: удержанные ответы уходят, `Live` удаляется, когда ничего не в полёте. Иначе: запрос в полёте переспрашивается через 10 с или сразу, если он ушёл на соединение, которое больше не соединение сессии (новый процесс агента, `/resume`). Удержанные ответы уходят по сроку или когда поток читать нельзя. Новое чтение только если нет запроса в полёте, срок `next_read` наступил, у сессии < 64 неотвеченных сообщений и всего ждут < 128 сообщений.
- `on_chunk(conn, session, chunk)`: принимается только ответ на свой запрос с того же соединения и с `from == read_at`. `missing`: одно предупреждение на эпизод, удержанные ответы уходят сразу (конца хода из отсутствующего файла не будет). `reset`: одно предупреждение на эпизод, `read_at = 0`, открытые вызовы сброшены. Иначе строки применяются по одной через `apply_line`; после первой строки чтение останавливается, если у сессии 64 неотвеченных или всего ждут 128 сообщений (остальное остаётся в файле, `read_at` на конце последней взятой строки). `Send` режется `split_for_telegram` на `Op::Stream` (склейка только однокусковым строкам вызовов), каждое сообщение считается в `queued_messages`. `Working` ставит ✍. `TurnEnd` отпускает самый старый ещё не отпущенный удержанный ответ ровно после сообщений до него; если удержанных не осталось (с учётом отпущенных этим же чтением), увеличивает `ends_unclaimed` (≤8). `NewTurn` и `Working` обнуляют `ends_unclaimed`. В конце барьер `(read_to, снимок calls)` и попытка зафиксировать смещение.
- `on_stream_done`: `queued_messages -= 1`. `Sent`/`Merged` = принято. 4xx от Telegram, кроме потерянной темы, = пропуск с одним предупреждением (сообщение, которое Telegram не возьмёт никогда, не должно навсегда остановить поток). Остальное (сеть, 5xx, потерянная тема, закрытый receiver склейки) = отказ: одно предупреждение на эпизод, барьеры за ним не фиксируются, когда ничего не в полёте, поток откатывается к зафиксированному барьеру и перечитывает через `stream_retry`.
- `stream_answered`: пишет `offset` и `calls` последнего пройденного барьера в registry (`dirty` только при изменении: холостой опрос `registry.json` не переписывает).
- `on_turn_answer`: если поток сессии может читать (`stream_target`): при `ends_unclaimed > 0` конец этого хода уже прочитан, ответ уходит сразу (и `ends_unclaimed -= 1`); иначе ответ удерживается (FIFO ≤8, лишний старейший уходит сразу) до `TurnEnd` или `hold_answer`, `next_read = now`. Пустой ответ стримящейся сессии тоже занимает свой конец хода (ничего не шлёт). Без потока: как TASK-022, сразу.
- `on_topic_message`: после успешного `try_send` квитанция в `stream.receipts` и 👀. `react` не больше 64 реакций в полёте, лишняя пропускается (debug); ошибка реакции только warn один раз, маршрутизация не меняется. Логи: короткий session id и фиксированный текст, без путей, id сообщений и текста.
- Тесты (20 stream-тестов в slots, из них 11 новых в ревью): порядок и частичная строка; рестарт hub без повторов и потерь; ротация с одним separator; `/clear` тем же процессом (тот же conn обслуживает B, строки A больше не идут); 👀/✍ только своя запись; запись чужого сервера не трогает реакцию; отказ реакции не ломает маршрутизацию; ответ после строк; ответ ждёт конца хода при отставании файла дольше старых 1,5 с; два `Stop` одного хода идут каждый после своих строк; два конца хода в одном чтении отпускают удержанный ответ и сразу следующий `Stop`; конец хода, прочитанный до `Stop`, отпускает ответ сразу; удержанный ответ уходит, если агент молчит; старый агент не спрашивается; холостое чтение не пишет registry; отказанное сообщение повторяется до сдвига смещения; отказанное сообщение приходит после рестарта hub; новый процесс агента продолжает без 10-секундного таймаута; усечённый файл перечитывается с начала; занятая очередь оставляет хвост чанка в файле.
- Существующий тест `a_topic_message_reaches_only_the_agent_of_its_slot` ждёт две реакции 👀 вместо нуля операций; тестовый `Fake` получил `react_error` и `stream_errors`.

**2.10 `crates/cctg/tests/stream_logs.rs`** (новый бинарь с глобальным subscriber): нет файла при ≥10 опросах = ровно одно «session transcript not found»; файл появился = строка ушла; файл усечён до нуля = ровно одно «was cut or replaced», новый файл читается с начала. Путь, имя проекта и текст промптов в логи не попадают. Логи читаются до удаления каталога.

**2.11 Не меняются:** `hook.rs`, `docs/hook-settings.json` (новых хуков нет), `channel::INSTRUCTIONS`, `wire::VERSION`.

## 3. Test plan

Ограничения хоста: один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз, каталог удалить в конце. Без Telegram, без `.env`/`device.env`, без интерактивного claude.

1. `cargo test -j 1 -p transcript --test stream`: 4 passed.
2. `cargo test -j 1 -p cctg --lib -- stream tail scheduler wire`: зелёные (узкий прогон при нехватке памяти).
3. `cargo test -j 1 -p cctg --test stream_logs`: 1 passed.
4. `cargo test -j 1 --workspace --no-fail-fast`: всё ok; `cctg` lib 358 passed / 1 ignored (ignored был до задачи). Эталон: `R/workspace_test.txt`.
5. `cargo fmt --all --check` и `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: чисто (`R/fmt.out.txt`, `R/clippy.out.txt`).
6. Мутации (`R/mutations.py`, для QA, не обязательны implementer-у): 35 мутаций (17 мутаций планировщика, адаптированных к новому коду, и 18 на исправления ревью), все KILLED; запуск `python R/mutations.py [префикс ...]` с теми же env. Результат: `R/mutations.out.txt`.

| Acceptance criterion | Чем закрыт |
|---|---|
| 1. дописанные строки -> turns в тему нужного слота по порядку | `appended_lines_reach_the_slot_topic_in_order_and_a_partial_line_waits`, `results_out_of_call_order_wait_and_go_in_call_order`, `a_turn_streams_its_prompt_notes_and_calls_but_not_its_final_answer`, `a_transcript_read_is_answered_over_the_link_and_never_reaches_claude` |
| 2. частичная строка не уходит и не теряется | `only_complete_lines_are_read_and_a_partial_one_waits_whole`, `bom_and_crlf_lines_keep_their_byte_offsets`, slot-тест с разрезанной строкой |
| 3. рестарт hub: без повторов уже принятого, без потерь | `a_restart_neither_repeats_nor_loses_stream_lines`, `a_refused_stream_message_comes_again_after_a_restart`, `the_offset_moves_only_over_accepted_messages`, `a_refused_message_stops_the_offset_until_the_stream_rewinds` |
| 4. планировщик: 20/мин, FIFO в теме, уступает permission | `group_limit_and_topic_order_hold` (общий bucket), `stream_lines_held_back_by_the_limit_merge_in_order_without_loss`, `a_permission_prompt_overtakes_the_stream_lines_of_its_topic`, `a_busy_queue_leaves_the_rest_of_a_chunk_in_the_file` |
| 5. ротация: новый поток, новое смещение, separator ровно раз | `a_new_session_in_the_slot_streams_after_its_one_separator`, `a_clear_in_the_same_process_streams_the_new_session_after_one_separator`, registry-тест |
| 6. нет/удалён файл: одно предупреждение, опрос дальше | `tests/stream_logs.rs`, `a_missing_file_or_a_foreign_path_is_missing`, `a_cut_transcript_is_read_again_from_its_start` |
| 7. 👀 при передаче, ✍ только по своей записи канала; терминал, UserPromptSubmit и чужой сервер не трогают; ошибка реакции не ломает маршрутизацию | `eyes_on_hand_off_and_writing_only_for_the_same_messages_channel_record`, `a_channel_record_of_another_server_leaves_the_reaction`, `only_cctg_channel_records_match_and_a_bom_line_still_counts`, `only_a_received_message_turns_to_working_and_only_once`, `a_refused_reaction_never_stops_routing`, `reactions_are_unmetered_and_the_newest_one_per_message_wins` |
| 8. вызов = одно сообщение-строка в порядке вызовов, при упоре склейка без потерь и порядка | `stream_lines_go_one_per_message_while_the_budget_has_room`, `stream_lines_held_back_by_the_limit_merge_in_order_without_loss`, `a_merged_message_stays_within_the_telegram_limit`, `a_refused_merged_message_answers_none_of_its_lines_as_merged`, `results_out_of_call_order_wait_and_go_in_call_order` |
| 9. замер лага и выбор источника | `OPEN_DECISIONS.md` (planner, 2026-09-24), `scratch/planner/lag_probe.*`, `passive_lag.*`, `hook_cost.out.txt` |
| 10. existing tests | полный workspace-прогон |

Порядок ответа хода (не отдельный критерий, но следствие 1 и 8): `a_turn_answer_follows_the_lines_read_after_its_stop`, `a_turn_answer_waits_for_its_turn_end_even_when_the_file_lags`, `every_stop_of_a_turn_follows_its_own_tool_lines`, `a_turn_end_read_before_its_stop_lets_the_answer_go_at_once`, `a_held_answer_goes_out_when_the_agent_never_answers`.

Живой smoke (для QA/пользователя после merge, не для implementer): hub + одна интерактивная сессия с каналом по `docs/poc.md`, 2-3 Bash-вызова (из них два параллельных), сообщение из темы во время хода. Проверить: строки вызовов в порядке вызовов, 👀 затем ✍, ответ после строк. Посмотреть в транскрипте, какой записью Claude Code кладёт channel-сообщение, пришедшее во время хода (`queued_command` или meta `user`); если третьей формой, сообщение останется с 👀 без ложного ✍.

## 4. Rollout notes

- **Миграций нет.** `registry.json` остаётся `version: 1`; новое поле `sessions[..].stream` опционально. Старый hub на новом registry поле игнорирует. Registry от эталона планировщика нигде не развёрнут, его формат `PendingCall.result_end` не поддерживается и не нужен.
- **Wire.** `VERSION = 1`. Новое только за capability `Register.transcript_reads`; `reset` и `TurnEnd` additive. Живые старые агенты (сессии, начатые до обновления) не стримят до restart/resume сессии; ответы TASK-022 и 👀 работают, ✍ у них не появится.
- **Env.** Новых переменных hub нет. Агент читает `CLAUDE_CONFIG_DIR` (как Claude Code), иначе `USERPROFILE`/`HOME`. Сессия с `CLAUDE_CONFIG_DIR` (как в `docs/poc.md`) стримится, потому что агент наследует env своего claude.
- **Поведение, которое увидит пользователь.** В теме появляются `> prompt` терминальных промптов, текст до вызовов и строки вызовов; финальный ответ стримящейся сессии может прийти до 5 с позже (обычно ~0.3 с: ждёт конец хода в jsonl). `Agent`-вызов даёт строку `↳ Explore: … ✓` рядом с блоком TASK-015 (решение orchestrator).
- **At-least-once.** Крах hub или отказ Telegram повторяет сообщения последнего незафиксированного чтения (до 64 строк), потерь нет.
- **Откат.** Revert коммита; поле `stream` в registry старый код игнорирует.
- **PCTX.** Предложение «TASK-016 implemented» в `PCTX_PROPOSALS.md` от планировщика устарело (1,5 с, `result_end`, путь без root); orchestrator сворачивает его по этому плану, раздел 2. Новые предложения ревью 2 там же (надёжность `is_error`, `stop_reason` на каждой записи ответа, чужие channel-серверы).

## 5. Review notes

### Disconfirmation

Самый конкретный контрпример, проверенный первым: «отказ Telegram (не 429) на stream-сообщение всё равно двигает сохранённый offset, и строка теряется навсегда». Подтвердился в эталоне планировщика: `Done::Stream { session, number }` в `slots.rs` не нёс `delivery`, `on_done` звал `live.answered(number)` для любого исхода, а `scheduler.rs` отвечал всем склеенным `Outcome::Merged` даже при ошибке общего send. Закрыто (находка 1), доказательство: `a_refused_stream_message_comes_again_after_a_restart` (Fake отказывает всем stream-сообщениям, offset в `registry.json` не доходит до конца файла, после рестарта строка приходит) и мутации R2/R3. Контрпример ревью 1 (результаты не в порядке вызовов) тоже подтвердился и закрыт (находка 0).

### Находки PLAN_V2: воспроизведены и исправлены

| # | Находка | Воспроизведение (до исправления) | Исправление | Доказательство |
|---|---|---|---|---|
| 0 | Результаты не в порядке вызовов уходят в порядке результатов | `hub/stream.rs` планировщика: `Result` сразу давал сообщение | открытые вызовы в порядке вызовов, отпускается только готовая голова; prompt/note/конец хода отпускают ход | `results_out_of_call_order_wait_and_go_in_call_order`, R1 |
| 1 | Offset двигается после отказа Telegram, склейка отвечает `Merged` при ошибке | `Done::Stream` без `delivery`; `scheduler.rs` слал `Merged` всем | `Done::Stream{delivery}`, барьеры со снимком вызовов, `Refused` блокирует фиксацию, rewind через `stream_retry`; `Merged` только после успеха; 4xx (кроме потери темы) пропускается с одним warn | `a_refused_stream_message_is_sent_again_before_the_offset_moves`, `a_refused_stream_message_comes_again_after_a_restart`, `a_refused_merged_message_answers_none_of_its_lines_as_merged`, `a_refused_message_stops_the_offset_until_the_stream_rewinds`, M2, R2-R4 |
| 2 | Truncate зацикливает старый offset, замена читается с середины | `tail.rs` отвечал `from = len`, hub отвергал «out of place» и спрашивал тот же offset | `reset` при `from > len` или байте `from-1` не `\n`; hub warn раз за эпизод, читает с 0 | `a_shorter_or_rewritten_file_is_reset_and_read_again_from_its_start`, `a_cut_transcript_is_read_again_from_its_start`, `stream_logs`, R5, R6 |
| 3 | Нет настоящего барьера между поздними строками и ответом `Stop`; второй `Stop` выпускал первый | отставание jsonl больше 1,5 с или второй `Stop` | маркер `TurnEnd`, FIFO удержанных ответов, `ends_unclaimed` для конца хода, прочитанного раньше хука, граница `hold_answer` 5 с | `a_turn_answer_waits_for_its_turn_end_even_when_the_file_lags`, `every_stop_of_a_turn_follows_its_own_tool_lines`, `a_turn_end_read_before_its_stop_lets_the_answer_go_at_once`, `two_turn_ends_in_one_read_serve_the_held_answer_and_the_next_stop`, R7, R8, R16, R18 |
| 5 | ✍ по записи другого channel-сервера с тем же числом | в реальных транскриптах есть `webhook`, `plugin:fakechat:fakechat` | только тег с `source="cctg"` (и в queued attachment) | `only_cctg_channel_records_match_and_a_bom_line_still_counts`, `a_channel_record_of_another_server_leaves_the_reaction`, R14 |
| 6 | Первая строка с BOM теряется навсегда | `str::trim` не снимает U+FEFF, разбор падал, offset уходил дальше | снятие BOM в `stream_events` | тот же transcript-тест, `bom_and_crlf_lines_keep_their_byte_offsets`, R15 |
| 7 | Кадр может превысить `MAX_LINE` | лимит текста проверялся после добавления строки, id и число items не ограничены | проверка «влезает ли строка» до добавления, 256 items, вес 128 KiB (+32 на item), id до 256 байт, тексты до 16 KiB | `no_record_makes_a_chunk_longer_than_a_link_line`, R10 |
| 8 | Неограниченные чтения агента, stream/reaction вне бюджета очереди, молчаливое вытеснение вызовов | `spawn` + `spawn_blocking` на каждый запрос; stream не считался в `queued_messages`; `MAX_CALLS` выкидывал старейший вызов | один читатель с очередью 1; stream в `queued_messages`, новых строк нет при 128 ждущих (хвост чанка остаётся в файле); реакции до 64 в полёте; переполнение вызовов отпускает старейший по общим правилам | `a_busy_queue_leaves_the_rest_of_a_chunk_in_the_file`, `open_calls_are_bounded_without_blocking_later_ones`, R12, R13 |
| 9 | Слабый path gate | принимался любой `.../projects/x/<id>.jsonl` | канонический путь строго `<root>/<папка>/<id>.jsonl`, root = `CLAUDE_CONFIG_DIR/projects` или `~/.claude/projects` агента, обычный файл | `a_missing_file_or_a_foreign_path_is_missing`, `a_junction_out_of_the_projects_root_is_not_followed`, R9 |
| 10 | Не хватало acceptance-тестов | см. PLAN_V2 | 21 новый тест, в том числе `/clear` тем же процессом, новый процесс агента, поток после отказа | раздел 3 |
| 11 | Эталон от устаревшего baseline | `scratch/planner` от `2792661` | новый patch от `e7b16b6` (crates те же), свои hashes; `git apply` на чистом `git archive HEAD` с `autocrlf=true`: 23 `OK` | `R/verify_hashes.sh` |

Сверх находок, при собственной попытке сломать порядок и смещения:
- Чтение, ушедшее на пропавшее соединение, ждало 10 с `READ_TIMEOUT`, а ответ старого соединения мог быть принят за ответ нового. Теперь запрос помнит соединение, переспрашивается сразу при смене соединения сессии, чанк принимается только с того же соединения (`a_new_agent_process_goes_on_from_the_stream_position`, R11).
- Два конца хода в одном чтении при одном удержанном ответе теряли второй, и следующий `Stop` ждал 5 с. Исправлено счётчиком отпусканий (`two_turn_ends_in_one_read_serve_the_held_answer_and_the_next_stop`, R18).
- Пустой `Stop` стримящейся сессии не занимал свой конец хода, и ответ следующего хода мог уйти раньше его строк. Теперь занимает.

### Отклонено или сделано иначе, чем в PLAN_V2

- **Находка 4 (ошибка инструмента как ✓) не воспроизведена, код не менялся.** Проверено на 9 661 реальном `tool_result` из 400 последних транскриптов этой машины: все 10 результатов `<tool_use_error>` и все 313 результатов со строковым `toolUseResult` (`Error:`, `User rejected`) несут `is_error: true`. `is_error` отсутствует только у `tool_reference` (ToolSearch) и у MCP-результатов; вывод ошибки из текста пометил бы удачные MCP-вызовы ✗. Предложение уточнить transcript domain лежит в `PCTX_PROPOSALS.md`.
- **Request id в wire не добавлен.** Поздний ответ после таймаута несёт `from`; hub берёт чанк только при `from == read_at` и только с соединения, куда ушёл запрос. Устаревший ответ либо остаётся верным чтением тех же байтов, либо отбрасывается.
- **Идентичность файла (windows-sys file id, dev/ino, checkpoint 4 KiB) не добавлена.** Её заменяет проверка «байт перед offset это `\n`»: переносимо, одно лишнее чтение байта. Claude Code не переписывает транскрипты; остаток описан ниже.
- **«Остановить read-ahead при заполнении очереди вызовов» (PLAN_V2) отклонено:** маркер, который отпускает ход (prompt, note, конец хода), лежит в файле дальше, остановка чтения дала бы вечное ожидание. Вместо этого 65-й открытый вызов отпускает старейший.
- **Корреляция `Stop.prompt_id` и `TurnEnd.prompt_id` отклонена:** у assistant-записей нет `promptId` (проверено на реальных jsonl), наличие `prompt_id` у `Stop` не проверено. Используется порядок: FIFO удержанных ответов и счёт концов хода.
- **«Никакого таймаута у удержанного ответа» (PLAN_V2) отклонено:** у `Stop` может не быть прочитанного конца хода (resume с конца файла посреди хода, отставание jsonl, рассинхронизация после рестарта). Граница `hold_answer` 5 с (замер: последняя запись хода видна через ~0,15 с), остаток описан.
- **`origin.kind == channel` для queued attachment не требуется:** форма не наблюдалась; проверки тега `source="cctg"` достаточно, и она не зависит от неподтверждённого поля.
- **`MAX_RECEIPTS` 32 с вытеснением оставлен:** вытесненное сообщение остаётся с 👀 (ложного ✍ нет, маршрутизация не затронута); квитанции потребляются каждым взятым сообщением.
- **Отдельный тест «не больше одного блокирующего чтения» не написан:** это структурное свойство (`mpsc::channel(1)` + один цикл чтения в `spawn_reader`), тест на тайминге был бы хрупким.

### Остаточные риски

- **At-least-once.** Крах hub или отказ Telegram (сеть, 5xx, удалённая тема) повторяет сообщения последнего незафиксированного чтения, включая уже принятые после отказанного (до 64 строк). Потерь и нарушения порядка нет.
- **Сообщение, которое Telegram отвергает 4xx** (кроме потери темы), пропускается с одним warn, иначе поток встал бы навсегда. При 403 (бота удалили) строки теряются, но доставить что-либо и так нельзя.
- **Удалённая тема.** Stream-отправки не запускают замену темы (как и reply); поток повторяет чтение раз в 5 с, пока тему слота не заменит другой путь. Нагрузка ограничена одним чтением и 64 сообщениями.
- **Удержанный ответ.** Если jsonl отстаёт больше 5 с, ответ уходит по сроку и поздние строки хода придут после него. Удержанный ответ живёт в памяти: рестарт hub в эти 5 с его теряет (как любое сообщение в очереди dispatch и до TASK-016).
- **Рассинхронизация концов хода.** После рестарта hub или rewind посреди хода перечитанный конец хода может засчитаться лишним; следующий `Stop` до нового промпта (блокирующий Stop-хук пользователя) тогда уйдёт без ожидания. Новый prompt или взятое Telegram-сообщение обнуляют счёт.
- **Незавершённые вызовы.** Вызов без результата к концу хода (или 65-й открытый) не показывается. У Claude Code так бывает только при прерванном вызове без записи результата.
- **Заменённый файл**, у которого байт перед offset случайно `\n`, читается с середины (начало замены теряется). Claude Code файл транскрипта не заменяет.
- **Форма channel-сообщения, пришедшего во время хода**, не наблюдалась: неизвестная форма оставит 👀 без ложного ✍ (smoke после merge).
- **`/clear`:** строки, дописанные в файл старой сессии после её `SessionEnd`, не стримятся (сессия больше не текущая в слоте).
- **Сервер зарегистрирован не под именем `cctg`:** тег получит другой `source`, ✍ не появится, 👀 останется.

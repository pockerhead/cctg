# TASK-016 PLAN V2: живой поток хода в тему слота

## 1. Review notes

### Проверенный контрпример

Контрпример для обязательной disconfirmation-проверки: транскрипт содержит `Call(A), Call(B), Result(B), Result(A)`. Требование — показывать завершённые инструменты в порядке вызовов.

Контрпример подтвердился. В эталоне `scratch/planner/ws/crates/cctg/src/hub/stream.rs:94-106` результат немедленно превращается в сообщение для найденного id, поэтому наружу уйдут B, затем A. Тест `a_call_line_goes_out_with_its_result_in_call_order` проверяет только результаты A, затем B и не подтверждает своё название. Исправление должно хранить позицию вызова и выпускать готовые строки через упорядоченный барьер, а не в порядке прихода результатов.

### Ошибки и пробелы исходного плана

1. **Offset продвигается после отказа Telegram.** `slots.rs:1938-1942` вызывает `live.answered(number)` для любого результата dispatch, не проверяя `Delivery`. Поэтому 400/500/сетевой отказ считается доставкой. Дополнительно `scheduler.rs:509-514` отвечает всем склеенным jobs `Outcome::Merged`, даже когда общий `sendMessage` завершился ошибкой. Это прямо противоречит заявленному смыслу offset — «байты, сообщения которых Telegram принял» — и может безвозвратно потерять строки.

2. **Усечение/замена файла зацикливает старое смещение.** `tail.rs:61-63` при `from > len` начинает с нового EOF и отвечает этим новым `from`; `slots.rs:1278-1285` отвергает ответ как «out of place» и снова спрашивает старый offset. После truncate hub не восстанавливается. Замена файлом той же или большей длины ещё хуже: чтение продолжается из середины нового файла и теряет его начало. В протоколе нет поколения файла или контрольной точки.

3. **Последний ответ не имеет настоящего барьера.** `slots.rs:1080-1123` держит Stop-ответ только до первого post-Stop read либо 1,5 секунды. Если jsonl отстал дольше, строка инструмента придёт после финального ответа. Повторный Stop немедленно выпускает предыдущий held answer. Тест проверяет лишь случай, где строки уже записаны до Stop, и не моделирует задержанную запись или несколько Stop одного turn.

4. **Ошибка инструмента может стать `✓`.** `transcript/src/stream.rs:82-91` считает ошибкой только `is_error == true`, хотя нормативный transcript domain отмечает, что `is_error` часто отсутствует, а ошибки также проявляются как `<tool_use_error>` и строковый top-level `toolUseResult`. Нужна консервативная классификация этих реальных форм.

5. **✍ коррелируется не со своим каналом.** `channel_message_id` проверяет только числовой `message_id`, но не `source="cctg"`. Meta-запись другого channel-сервера с тем же числом может изменить реакцию. Для queued attachment также надо требовать `origin.kind == "channel"` и собственный source.

6. **BOM теряет первую запись.** Tail передаёт строку как есть, а `stream_events` делает только `trim`; U+FEFF не является JSON whitespace. CRLF проходит, но BOM-prefixed строка пропускается и offset навсегда проходит её. Это не покрыто новыми тестами.

7. **Заявленные wire-границы не обеспечены.** `MAX_CHUNK_TEXT` проверяется после добавления целой строки. `Prompt`/`Note` обрезаются, но tool id не ограничен; одна внешняя jsonl-строка с большим id или множеством blocks может породить frame больше `wire::MAX_LINE`. Требуется ограничивать именно сериализованный ответ до добавления строки.

8. **Чтения и dispatch фактически не ограничены.** Каждый `TranscriptRead` создаёт новый `tokio::spawn` + `spawn_blocking` (`agent.rs:462-471`). Таймаут hub способен породить перекрывающиеся неотменяемые blocking reads. Tokio прямо предупреждает, что `spawn_blocking` не отменяется и параллелизм надо ограничивать: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html. Stream и reaction jobs обходят существующий `MAX_QUEUED_MESSAGES`, хотя попадают в `mpsc::unbounded_channel` (`slots.rs:374-376`). `MAX_CALLS` и `MAX_RECEIPTS` ограничены, но молча выкидывают старые записи, то есть достигают boundedness ценой нарушения функциональных гарантий.

9. **Path gate слабее заявленного.** `is_transcript_path` принимает любой путь формы `.../projects/x/<session>.jsonl`, а не только локальный Claude projects root. Значит аутентифицированный, но ошибочный hub request может заставить агент разобрать и отправить содержимое постороннего похожего файла. Нужна привязка к `<CLAUDE_CONFIG_DIR|~/.claude>/projects`, canonical containment для существующего файла и защита от `..`/symlink/junction escape.

10. **Покрытие acceptance неполно.** Нет тестов на out-of-order results, доставку с non-429 ошибкой, merged-send error, truncate/replace/delete-after-open, BOM, `/clear` с тем же agent process, restart только агента, stale reply после read timeout, delayed transcript after Stop, несколько Stop, очередь/reader caps и foreign channel source. Тест «missing» проверяет только отсутствие до создания файла.

11. **Формулировка baseline устарела, хотя patch применим.** Текущий HEAD — `fc746ad`, не `2792661`; `2792661` является предком, после него закоммичен план. `git apply --check scratch/planner/task016.patch` на текущем HEAD проходит. Hashes подтверждают только исходный эталон и после необходимых исправлений намеренно перестанут совпадать; их нельзя использовать как финальный критерий.

### Что в исходном плане подтверждено

- Решение из `OPEN_DECISIONS.md` остаётся: источником является transcript tail на агенте; Pre/PostToolUse hooks не добавляются. Замеры действительно показывают `tool_result` примерно через 0,1–0,2 с после завершения инструмента, тогда как отдельный hook-процесс стоит около 0,5 с при недоступном hub.
- Additive capability в `Register`, optional/default wire fields и `VERSION = 1` соответствуют правилу rolling compatibility.
- Форма Bot API вызова в `api.rs` верна: `reaction` — массив `ReactionType`, бот может установить одну реакцию; `👀` и `✍` входят в официальный список. Источник: https://core.telegram.org/bots/api#setmessagereaction и https://core.telegram.org/bots/api#reactiontypeemoji.
- Slots actor в эталоне не читает файл и не ждёт Telegram на своём потоке; blocking IO вынесен на агент. Это надо сохранить.
- Все 23 файла patch относятся к задаче: новые stream/tail модули, wire/agent/api/scheduler/slots/registry и целевые тесты. Малые правки в четырёх log tests лишь дополняют `Register`. Удалять их из scope не требуется.

## 2. Updated understanding

- Реальный source baseline — текущие `crates/**` на HEAD `fc746ad`; TASK-016-кода в них ещё нет. `scratch/planner/ws/` — компилируемый прототип поверх source-состояния `2792661`, а не готовый к merge результат.
- TASK-022 уже отправляет непустой `Stop.last_assistant_message` через общий outbound path. TASK-016 не должен повторно отправлять `end_turn` text из jsonl; jsonl нужен как барьер и как источник prompt/note/tool events.
- Hub знает `transcript_path` из SessionStart, но файл физически доступен агенту устройства. После `/clear` тот же MCP-процесс привязывается к новой session по pid и должен обслуживать путь новой session. При новом process `/resume` persisted cursor остаётся у hub.
- Scheduler — единственная точка Bot API writes. В нём уже есть общий token bucket и сериализация 429. Stream-сообщения должны быть metered, reactions — сериализованные unmetered edits, но любой 429 продолжает останавливать всю очередь.
- `registry.json` — единственное durable состояние потока. Durable cursor означает только подтверждённый Telegram-prefix; read-ahead, request ids и jobs in flight остаются оперативным состоянием и после падения могут дать повтор, но не пропуск.
- Порядок должен определяться transcript position: prompt/note и placeholders вызовов образуют одну последовательность. Channel receipt — out-of-band сигнал реакции и не должен блокироваться ожиданием более раннего tool result.
- Решения orchestrator закрыты и уже встроены в этот план: terminal prompts показываются как `> text`; строка `Agent` остаётся рядом с TASK-015 block; queued-channel shape проверяется реальной fixture либо live QA; hook source не используется.

## 3. Revised approach

### Источник и wire compatibility

Оставить agent-side polling. `Register.transcript_reads: bool` — additive capability с `#[serde(default)]`; `VERSION` остаётся 1. Старый agent сообщает false/не содержит поле и не получает requests. Новый agent со старым hub лишь передаёт неизвестное поле Register, которое старый serde decoder игнорирует, и requests не получает.

Расширить request/response идентификатором запроса и состоянием файла:

- `TranscriptRead { request_id, session_id, path, cursor }`;
- `TranscriptChunk { request_id, session_id, status, from, to, generation, checkpoint, lines, more }`;
- optional/default поля сохраняют чтение старых frames, но hub принимает chunk только для точного current `request_id`, connection binding и current live top-level session;
- `status` различает `ok`, `missing`, `reset` (truncate/replace) и `unreadable`. Ошибки не несут path или OS error text.

Persisted cursor содержит `offset`, file generation и checkpoint последних максимум 4 KiB перед offset. На Windows generation берётся из file identity через уже используемый `windows-sys` (добавить только нужный feature); на Unix — `dev/ino`; checkpoint страхует truncate-and-regrow той же inode/file id. Несовпадение identity/checkpoint или `len < offset` даёт `reset`, после чего hub предупреждает один раз за episode, очищает относящиеся к старому поколению pending calls и перечитывает новый файл с 0. Повтор возможен, потеря начала replacement — нет.

### Agent tail

- Один bounded reader worker на agent process (capacity 1, одновременно ровно одно `spawn_blocking`); stdio/channel loop только `try_send`-ит request и никогда не ждёт файл. Дубликат exact request можно отклонить/заменить, но нельзя создавать неограниченные blocking tasks.
- Разрешён только canonical файл под `<CLAUDE_CONFIG_DIR|~/.claude>/projects/<project>/<session_id>.jsonl`; для missing path сначала проверяется lexical containment, для существующего — canonical containment. Session id остаётся plain и совпадает с basename.
- Читать только newline-terminated records. CRLF разрешён; UTF-8 BOM удаляется перед разбором первой/любой record, но byte offsets считаются по исходным байтам. Partial record оставляет `to` перед своим началом.
- Лимиты применяются до добавления `StreamLine`: не более 4 MiB scan, 64 eventful lines и такой encoded JSON budget, чтобы весь frame гарантированно был `< wire::MAX_LINE`. Ограничить id/line/error и число items одной record; oversized record пропустить целиком с privacy-safe status/counter, не раздувая память.

### Stream extraction and ordering

`transcript::stream_events` остаётся чистым и переиспользует `user_text`/`tool_line`, но добавляет `TurnEnd { prompt_id }` без final text. Для реакции `Channel` создаётся только если opening tag имеет `source="cctg"`, numeric `message_id`, а queued attachment также имеет channel origin. Текст Telegram prompt никогда не едет как reaction evidence.

Tool failure определяется по `is_error: true`, `<tool_use_error>…</tool_use_error>` либо строковому top-level `toolUseResult`, начинающемуся с error marker. Неизвестная форма не должна выдумывать error text; она даёт только `✗` при доказанном failure.

Hub хранит упорядоченную очередь stream events с call placeholders:

- `Call` фиксирует transcript position и brief line;
- `Result` только помечает соответствующий placeholder готовым;
- topic messages выпускаются с головы очереди; B не обгоняет незавершённый A;
- на matching `TurnEnd` готовые calls выпускаются в call order, а вызовы без результата считаются незавершёнными и не получают ложного `✓`;
- final text из TurnEnd не отправляется.

State ограничен (например, 1024 pending placeholders/events на session). При достижении cap hub прекращает read-ahead и оставляет cursor перед первой непоместившейся record: durable jsonl служит backpressure queue. Нельзя молча `remove(0)` и забывать call/receipt.

### Delivery and cursor commit

Каждый прочитанный chunk образует ack barrier. Сообщения chunk получают sequence numbers, но durable cursor двигается к `to` только когда:

1. состояние pending calls/receipts сохранено в snapshot;
2. все topic messages перед barrier получили `Ok(Outcome::Sent | Outcome::Merged)`;
3. merged head также успешно доставлен.

Промежуточный success не должен продвинуть offset за ещё не доставленное сообщение, даже если call-order отличается от byte-order. На non-429 error barrier остаётся uncommitted; stream повторяет job с bounded backoff. Неоднозначный сетевой исход допускает at-least-once duplicate. `TOPIC_ID_INVALID`/thread-not-found запускает существующее восстановление topic, затем retry. Scheduler при ошибке merged head сообщает failure всем merged receivers; `Merged` выдаётся только после успешного общего send.

Stream jobs входят в существующий глобальный `MAX_QUEUED_MESSAGES = 256`. При заполнении hub останавливает reads, не выбрасывает строки: они остаются в jsonl за committed cursor. Reactions имеют отдельный небольшой bounded/coalescing budget; их overflow/failure не меняет routing и логируется один раз за episode без ids/text/path.

### Stop ordering

Вместо одного `Held` хранить bounded FIFO Stop answers. `Stop.prompt_id` связывается с `TurnEnd.prompt_id`; пока agent/file доступны, финалы этого turn выпускаются только после соответствующего transcript barrier и всех более ранних stream messages. Несколько Stop одного prompt сохраняются и выходят после tool lines в hook order — без dedup, как требует TASK-022.

Если transcript заведомо недоступен (`missing`, unreadable, old agent, disconnect), final answer не блокируется: он идёт обычным TASK-022 path. Пока файл доступен, использовать matching end marker, а не фиксированный timeout: произвольный deadline снова разрешил бы поздним tool lines перескочить за final либо заставил бы их потерять. Held FIFO и общий pending budget остаются ограниченными; при их заполнении прекращается read-ahead/приём новых stream jobs, а не нарушается порядок уже принятого turn.

### Reactions

После успешного `try_send(Inbound)` ставить 👀 независимо от transcript capability. Receipt сохраняет `message_id` текущей session. ✍ создаётся только по `Channel { source=cctg, message_id }` той же session и только если id есть среди receipts. `UserPromptSubmit`, terminal prompt, foreign channel и unmatched id ничего не меняют.

`BotApi::set_message_reaction` оставляется в проверенной форме:

```json
{"chat_id":-1000000000000,"message_id":42,"reaction":[{"type":"emoji","emoji":"✍"}]}
```

В fixtures/логах использовать только синтетические chat/message ids. Restriction `available_reactions` и старые сообщения могут дать `REACTION_INVALID`; это допустимый failure реакции, не routing.

## 4. Revised steps

1. **Зафиксировать baseline.** Проверить `git status --short`, текущий HEAD и `git apply --check scratch/planner/task016.patch`. Не требовать HEAD ровно `2792661`. Patch можно применить как scaffold, но `verify_hashes.sh` используется только до исправлений; после шагов ниже mismatch ожидаем.

2. **Добавить чистое извлечение событий в transcript crate.** В `crates/transcript/src/stream.rs`, `lib.rs`, `render.rs` реализовать Prompt/Channel/Note/Call/Result/TurnEnd, строгий `source=cctg`, channel-origin queued attachment, error inference и BOM/CRLF tolerance. Не добавлять IO/dependencies; обновить purity scan. Fixture должна быть анонимной и включать multi-block, absent `is_error`, foreign source, BOM и final marker.

3. **Определить additive wire contract.** В `wire.rs` добавить capability, request id, cursor/generation/checkpoint/status и bounded StreamItem shapes с defaults. Оставить `VERSION = 1`. Тестами доказать: old Register -> false; Register нового agent читается decoder-ом без нового поля; old agent не получает unknown request; unknown newer StreamItem пропускается; frame всегда меньше `MAX_LINE`.

4. **Реализовать безопасный bounded tail.** В `tail.rs` валидировать allowed root и canonical containment, получать identity/checkpoint, различать missing/reset/unreadable, читать только complete lines и соблюдать encoded budget. Partial line, CRLF и BOM не теряются. Truncate/rewrite/replacement возвращают reset, а не несовпадающий обычный chunk.

5. **Сериализовать agent file reads.** В `agent.rs` заменить per-request `spawn_read` на один bounded worker; channel loop не ждёт IO. Echo exact `request_id`. При reconnect response старого request можно потерять, hub повторит его; параллельные file reads не создаются. `channel.rs` продолжает не отдавать TranscriptRead Claude.

6. **Расширить durable registry.** В `registry.rs` хранить cursor generation/checkpoint, ordered placeholders, bounded receipts и необходимые barrier metadata с `#[serde(default)]`. Startup/clear начинают новый файл с 0; впервые увиденный resume — с EOF; resume известной session сохраняет cursor; nested session stream не получает. При load нормализовать новые vectors к cap без логирования содержимого.

7. **Переделать reducer потока.** В `hub/stream.rs` сделать очередь по transcript position, результат отмечает placeholder, drain соблюдает call order и общий event order. Убрать silent eviction. Ack model должен коммитить chunk целиком только после всех зависимых deliveries. Channel receipts обрабатываются out-of-band, exact once per stored id.

8. **Исправить scheduler semantics.** В `scheduler.rs` сохранить общий 20/min bucket, per-topic order, permission priority и merge только соседних mergeable tool lines одной темы до 4096 UTF-16. На success merged followers получают `Merged`; на failure все followers получают failure/closed receiver. Добавить stream jobs в общий pending accounting; reaction coalescing остаётся в edit lane, 429 ставит на паузу всё.

9. **Интегрировать в Slots без IO/await.** В `slots.rs` принимать только chunk с current request id/connection/session, управлять reset episodes, backpressure, barrier deliveries, retries и topic replacement. Не двигать cursor на `None`/`Err`. Stream стартует только после принятого session separator. `/clear` переносит тот же connection к новому session/path, старый stream больше не читает.

10. **Интегрировать Stop barrier.** Хранить FIFO held answers по prompt id; matching TurnEnd отпускает их после stream jobs. Old/non-capable/disconnected/missing path использует немедленный TASK-022 fallback. Не отправлять final assistant text из transcript.

11. **Интегрировать reactions.** Добавить проверенный API method и scheduler op. На successful inbound enqueue — 👀; на exact cctg channel record той же session — ✍. Ошибка/overflow reaction work не влияет на inbound и stream. Не логировать message id, user id или текст.

12. **Добавить целевые тесты transcript/tail/wire.** Минимальный набор:
    - `out_of_order_results_wait_and_emit_in_call_order`;
    - `an_absent_error_flag_with_tool_use_error_is_failed`;
    - `only_cctg_channel_records_match_a_receipt`;
    - `bom_crlf_and_partial_records_preserve_byte_offsets`;
    - `truncate_replace_delete_and_reappear_reset_once_and_continue`;
    - `oversized_record_cannot_exceed_max_line`;
    - `canonical_path_cannot_escape_projects_root`;
    - `one_agent_has_at_most_one_blocking_read`;
    - direct old/new VERSION=1 compatibility tests.

13. **Добавить scheduler/reducer tests.** Проверить 20 sends в любом 60-second window, FIFO каждой темы, permission перед backlog stream, отдельные tool messages при наличии tokens, lossless merge при exhaustion, 4096 UTF-16, failure merged head не ack-ит followers, non-429 failure не двигает cursor, global pending cap останавливает read-ahead.

14. **Добавить Slots integration tests.** Использовать temp jsonl и fake Telegram:
    - append order + partial line;
    - hub restart: только acked cursor, записи во время downtime читаются;
    - agent disconnect/reconnect и новый process `/resume` продолжают cursor;
    - `/clear` с тем же pid создаёт новый stream после ровно одного separator;
    - stale chunk старого request id игнорируется;
    - deletion/truncate/replacement предупреждают один раз за episode и polling продолжается;
    - delayed tool result и TurnEnd после старых 1,5 с всё равно идут до Stop answer;
    - несколько Stop одного prompt идут после tools и не dedup-ятся;
    - transcript final text не дублирует TASK-022 answer;
    - 👀/✍ exact id, terminal/unmatched/foreign source untouched, reaction failure routing не ломает;
    - old agent never receives reads, new agent with old-message decoder не требует VERSION bump;
    - stalled Telegram не заставляет Slots ждать и не создаёт unbounded work.

15. **Проверить privacy logs отдельным integration binary.** Missing/reset/read error/retry/overflow/reaction failure должны логировать только short session и фиксированный reason. Отрицательно проверить transcript path/project name, prompt/tool text, Telegram ids, allowed user ids, token и hub secret. Не читать `.env`/`device.env`.

16. **Прогнать проверки с ограничениями хоста.** Один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo одновременно. Сначала узкие тесты `-p transcript --test stream`, затем `-p cctg --lib tail`, `hub::stream`, `hub::scheduler`, Slots filters и log binary; после них `cargo test --workspace --no-fail-fast`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`. В конце удалить только явно созданный temp target.

17. **QA без реального API в implementation stage.** Не вызывать Telegram, не запускать interactive Claude и не читать private env. После merge отдельный пользовательский smoke из `docs/poc.md` проверяет только ещё не наблюдавшуюся форму channel message, queued during turn; если реальная fixture доступна раньше, анонимизировать минимальный slice и закрыть это тестом.

### Acceptance mapping

| Критерий | Реализация и доказательство |
|---|---|
| Append -> правильный slot/order | ordered event queue + Slots append integration |
| Partial line | complete-line tail test с двумя poll |
| Hub restart без пропуска | durable ack barrier + restart/downtime test |
| 20/min, FIFO, permission priority | scheduler paused-time tests и merge failure test |
| Rotation/new offset/one separator | explicit `/clear` same-pid integration |
| Missing/deleted file | status episodes + delete/reappear test и privacy log test |
| 👀/✍ exact message | receipt tuple/current session + own-source parser tests |
| Tool per message/call order/lossless merge | out-of-order result test + token/merge tests |
| Source decision measured | committed `OPEN_DECISIONS.md` + existing lag/hook-cost artifacts |
| Existing tests | full workspace, fmt, clippy |

## 5. Risk areas

- **At-least-once, не exactly-once.** Если Telegram принял send, но HTTP answer потерян, retry может дать duplicate. Cursor не должен двигаться без наблюдаемого success, поэтому предпочтение отдаётся повтору, а не потере.
- **Форма queued channel во время активного turn всё ещё не наблюдалась.** Поддерживать только подтверждённый meta-user shape и строго распознанный queued attachment. Неизвестная форма оставит 👀; она не должна дать ложный ✍.
- **Stop fallback.** При реально отсутствующем transcript final answer отправляется без stream barrier и tool lines этого недоступного источника гарантировать нельзя. Пока файл доступен, matching TurnEnd обязателен: timeout, который выпускает final раньше, запрещён, потому что он снова создаёт исходную ошибку порядка.
- **Bounded pending state.** Jsonl является durable overflow queue. При достижении лимита поток замедляется/останавливает read-ahead, а не забывает события. Это может задержать длинный патологический turn, но сохраняет память и данные.
- **File identity portability.** Windows — основной target и должен иметь надёжный file id. Unix использует dev/ino; fallback checkpoint нужен для файловых систем без стабильного identity. Truncate-and-regrow проверяется checkpoint, а не только длиной.
- **Rolling upgrade.** Уже живые старые agents не стримят до restart/resume; финальные ответы TASK-022 и 👀 остаются работоспособными, ✍ без transcript capability недоступен. VERSION не меняется.
- **Общий Telegram budget.** Prompt/note/final/reply делят 20/min со stream. Merge уменьшает число только tool messages и никогда не пересекает ordinary message или session separator.
- **Reactions restricted by chat.** `REACTION_INVALID`, старое сообщение или отсутствие права — ожидаемая мягкая ошибка. Она не должна удерживать offset или routing.
- **Scope.** Не добавлять hooks, Node, новый MCP framework, watcher crate или remote filesystem abstraction. Сохранить 23-файловую хирургическую область; новые platform-specific строки допустимы только внутри tail/file-identity реализации и её тестов.

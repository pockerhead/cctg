# TASK-015 PLAN V2: субагенты и вложенные запуски внутри слота родителя

## 1. Review notes

Проверен reference workspace `scratch/planner/ws/`, patch `scratch/planner/task015.patch` и шесть файлов из `hashes.txt`: hash всех файлов совпадает, `git apply --check` на текущем HEAD проходит. Представленный `workspace_test.txt` действительно содержит успешный workspace-run (cctg: 296 passed / 1 ignored), однако несколько тестов закрепляют ошибочную семантику, поэтому зелёный прогон не доказывает выполнение всех критериев.

### Проверенный контрпример

Контрпример: hub сохраняет `Block.sending = true`, Telegram принимает `sendMessage`, но hub падает до получения/сохранения `message_id` — либо запрос вообще не дошёл. После рестарта обе ситуации выглядят одинаково.

Он подтвердился в коде: `registry.rs:1082-1085` при `sending=true && message_id=None` безусловно очищает `pending` и ставит `running=false`. Тест `blocks_survive_a_restart_without_a_second_send` ожидает, что такой блок останется без `message_id` и больше никогда не будет отправлен. Значит дубля действительно нет, но блока в Telegram может не быть и пометить его `итог не получен` невозможно. Это противоречит безусловной фразе плана «restart neither loses nor repeats it» и буквальному прочтению acceptance criterion 6.

Bot API не даёт `sendMessage` idempotency key: идентификатор сообщения появляется только в успешном ответе метода; `random_id` относится к MTProto, не к HTTP Bot API. Поэтому одновременно гарантировать «никогда не дублировать» и «никогда не терять при неизвестном исходе send» нельзя. Выбранная и подлежащая документированию политика — at-most-once: известные (`message_id` сохранён) незавершённые блоки детерминированно редактируются, неизвестный исход первого send становится детерминированным tombstone в registry и не пересылается. Источники: [Telegram Bot API](https://core.telegram.org/bots/api), [MTProto updates/random_id](https://core.telegram.org/api/updates).

### Другие проблемы исходного плана

1. **Старые registry могут снова породить ghost block.** У старого `SubagentEntry` нет `block`; `#[serde(default)]` загружает пустой `Block`. После нового typed stop `on_subagent` считает запись подтверждённой, `read_body` формирует текст, а `block_work` впервые отправляет его. Утверждение плана «old ghosts stay harmless» неверно. При загрузке V1 надо удалить/мигрировать legacy subagent entries с пустым header, не доверять им как скоррелированным.

2. **Новая очередь блоков не ограничена.** `Slots` и так использует unbounded dispatch (`slots.rs:324-326`), а `pump` передаёт все результаты `registry.block_work()` как `Work::Block`. Эти jobs намеренно не учитываются в `MAX_QUEUED_MESSAGES`; произвольное число подтверждённых субагентов при зависшем Telegram растит память. Pending-состояние уже есть в registry, поэтому dispatch должен получать только ограниченное число jobs.

3. **Индекс корреляции растёт всю жизнь активной сессии.** `AgentIndex.calls/links` не имеет лимита, а `Done::Index` удаляет индекс только для не-live top-level сессии (`slots.rs:1546-1551`). Долгая живая сессия накапливает все Agent calls/results. Нужны численные пределы и eviction, сохраняющий записи активных candidates.

4. **Чтения body не coalesce-ятся и могут нарушить fallback.** Каждый повторный stop для уже известного агента запускает новый `spawn_blocking` до 64 MiB. Результаты могут завершиться не по порядку; старый body тогда затрёт новый. Кроме того, первый запуск делает `reports.take`, поэтому более поздний duplicate stop может перезаписать доставленный handback-report fallback-ом более низкого приоритета. Нужны latest-wins generation, максимум одно чтение на block (и малый глобальный лимит), а report следует удалять только после принятия результата с максимальным приоритетом.

5. **Running header может превышать 4096.** `description` из parent Agent input не ограничен, а `fit()` вызывается только для финального body. Начальный `header + в работе…` может пять раз получить `message is too long` и исчезнуть. Header/description надо нормализовать и обрезать до Telegram-лимита до записи в registry; финальный текст по-прежнему проходит `fit()`.

6. **Некоторые memory maps не имеют корректного lifecycle.** `nested_answers` живёт только в памяти до SessionEnd и теряется при рестарте между nested Stop и SessionEnd; это нарушает принятое решение показывать последний ответ nested run. Его надо хранить в persisted block state либо обновлять persisted pending text на Stop. Candidates/reports/body requests/indexes должны очищаться по ended/pruned session и иметь тестируемые пределы.

7. **Restart tests покрывают только удобный случай.** Actor-test создаёт оба сообщения и уже знает `message_id` до сохранения. Отдельно тестируется неизвестный send, но потеря объявлена успехом и не проверяется tombstone. Также нет migration test со старым `registry.json`, содержащим legacy subagent ghost.

8. **Research в целом подтверждает выбранную корреляцию.** Официальный hooks reference говорит, что internal `SubagentStop` реален, а его `agent_type` может совпадать с `--agent`; он же подтверждает `SubagentHandback.tool_input.message` и отдельный `last_assistant_message`. Значит фильтрация только по hook fields недостаточна, а связь через parent `Agent` result с `agentId` правильна. Источник: [Claude Code Hooks reference](https://code.claude.com/docs/en/hooks). Ключ `target_agent` допустим: channel meta keys ограничены буквами, цифрами и `_`; значение остаётся строкой. Источник: [Claude Code Channels reference](https://code.claude.com/docs/en/channels-reference).

## 2. Updated understanding

- В текущем repo HEAD реализации TASK-015 ещё нет; `scratch/planner/ws/` — применённый reference patch. Менять нужно только шесть названных patch-файлов: `hub/mod.rs`, `hub/registry.rs`, `hub/slots.rs`, новый `hub/subagents.rs`, `transcript/src/subagent.rs`, `transcript/tests/subagent.rs`.
- Hook layer уже отбрасывает blank `agent_type`, а typed stop пропускает только при наличии `.jsonl` или `.meta.json`. Этого недостаточно для typed `--agent`; окончательное доказательство явного субагента — пара parent `Agent` tool_use + соответствующий tool_result с `toolUseResult.agentId`.
- `transcript::Subagent::new` уже владеет правильным fallback: captured report → finished brief, согласованный с `last_assistant_message` → last message → in-progress/empty. Hub должен только передать консистентный snapshot и не позволить позднему слабому результату затереть сильный.
- Registry уже является единственным durable owner slot/session state. Nested session получает parent slot reference, но не занимает slot; `agent_connected` её отвергает. TASK-022 отправляет turn answer только через `current_slot`, поэтому nested Stop и SubagentStop не должны становиться отдельными turn answers.
- Все Telegram operations должны оставаться вне actor: actor лишь ставит ограниченное число jobs в dispatch. File IO остаётся в `spawn_blocking`, но также должно иметь ограниченную concurrency.
- Приняты решения orchestrator: subagents nested run не показываются; reply на finished subagent остаётся targetable; nested block показывает last answer; correlation window — 60 s; remote-device subagent blocks пока не поддерживаются.

## 3. Revised approach

1. **Candidate first, durable confirmation only after correlation.** Typed start/stop top-level parent становится bounded candidate. Incremental scan parent transcript связывает только совпавшие `Agent` tool-use id и result `agentId`. Stop немедленно перезапускает 60-секундное окно. Никакое нескоррелированное событие не попадает в registry.

2. **Bound every new state and queue.** Оставить `MAX_CANDIDATES=256`, `MAX_REPORTS=256`; добавить пределы index entries, pending body reads, concurrent blocking reads и in-flight block jobs. Eviction не должен удалять сведения, нужные активному candidate; при переполнении сначала удаляются неактивные/старейшие записи, иначе новый candidate детерминированно отклоняется с content-free warn/debug. Registry subagents также получает общий предел с eviction старейших finished entries; это означает документированное прекращение `target_agent` routing для очень старых блоков.

3. **Single-writer body completion.** Для `(parent_session, agent_id)` держать latest `BodyInput` и generation. Одно чтение на key; новый stop/handback coalesce-ится. `Done::Body` применяется только если generation актуален; затем запускается накопившийся newest request. Report не уничтожается до успешного применения report-based body.

4. **Durable block state with explicit unknown-send state.** Persist header, pending/current text, running, thread/message id и first-send state. После рестарта:
   - `message_id=Some`: редактировать тот же message, никогда не send второй;
   - send не начинался: send один раз;
   - send был начат, ответа нет: не повторять, записать deterministic `delivery_unknown` tombstone в registry; Telegram-пометка невозможна и это явно отражается в тесте/риске.
   Legacy entries с пустым header удаляются при load. Известные running blocks ended sessions получают `итог не получен` edit.

5. **Nested block is persisted end-to-end.** Один `⇣ nested <short>` создаётся только для `Nested { parent: Some }` с parent slot. Nested Stop сохраняет bounded last answer в block state (не в ephemeral HashMap); SessionEnd завершает тем же ответом либо `· завершён`. `NestedUnknownParent`, own-pid guard и nested resume top-level id не создают block/topic/route.

6. **Reply routing stays narrow.** Только explicit Telegram reply на message id скоррелированного block, в том же thread и у live current top-level parent, добавляет ровно `target_agent=<agent_id>` и идёт через connection родителя. Finished block остаётся targetable. Nested/old-session/foreign-thread replies не получают meta; nested agent connection ничего не получает.

7. **Text safety and delivery.** Нормализовать type/description одной строкой и ограничить initial header так, чтобы running text укладывался в `telegram_len <= 4096`. Final body проходит существующий `fit`: preview edit/send плюс bounded document job для полного текста. Block sends/edits получают отдельный bounded in-flight budget; pending остаётся в registry, а не размножается в unbounded channel. Логи содержат только короткие session/agent ids и статические причины — без message text, paths, Telegram user ids и secrets.

## 4. Revised steps

1. **Transcript API (`crates/transcript/src/subagent.rs`, `tests/subagent.rs`).**
   - Добавить `description` fallback в `SubagentInput`, публичный `header()`, использовать его из `render()`.
   - Сохранить единственный authoritative fallback внутри `Subagent::new`.
   - Тесты: meta wins; call description fallback; blank fields; `header == first render line`; report > finished transcript > lagging transcript/last message. Не добавлять IO в transcript crate.

2. **Correlation helpers (`crates/cctg/src/hub/subagents.rs`, export в `hub/mod.rs`).**
   - Реализовать capped complete-line incremental scan, `AgentCall`, correlation index и candidate retry `[1,2,4,8,16,16…]` до 60 s; stop открывает новое окно немедленно.
   - Сохранить существующую проверку принадлежности `agent_id` одной parent session: повторный hook того же id от другой сессии игнорируется. Документация называет `agent_id` уникальным; менять durable key/schema без наблюдавшейся коллизии не нужно.
   - Ограничить candidate/report text уже существующим hook cap; добавить `MAX_INDEX_ENTRIES`, `MAX_PENDING_BODIES`, `MAX_BODY_READS` и deterministic eviction/coalescing.
   - Не держать полный index после исчезновения candidates; для нового candidate у той же сессии корректно пересканировать нужный диапазон/файл, чтобы bounded eviction не дал false negative.
   - `read_body` читает `.jsonl` до 64 MiB и meta до 64 KiB вне actor; missing/lagging file — штатный fallback.
   - `header` сразу ограничивает running message; `fit` ограничивает final preview и возвращает whole document.
   - Unit tests: call+result, обе половины отдельно и в разных scans, partial last line, truncation/rotation path, 60-second reopen, all bounds/evictions, hook того же id от другой parent session игнорируется, huge description, missing/lagging files, полный fallback.

3. **Durable model (`crates/cctg/src/hub/registry.rs`).**
   - Добавить defaulted V1-compatible block fields к nested session и confirmed subagent; `VERSION` не повышать.
   - На load нормализовать legacy subagents: запись без non-empty header считается старой нескоррелированной записью и удаляется. Добавить fixture-style serialization test старого V1 registry.
   - `confirm_subagent` принимает только known top-level parent со slot, проверяет совпадение parent для уже известного `agent_id`, создаёт один bounded running block и sequence для eviction.
   - Ввести явные состояния first send (unsent / outcome unknown / sent message id), bounded `block_work(limit)`, retry counters и persisted nested last answer.
   - `lose_blocks` редактирует только block с известным message id; unknown-send получает registry tombstone без повторной отправки. `after_restart` очищает только ephemeral busy/failed flags.
   - Ограничить число durable subagent entries; сначала удалять oldest finished, затем детерминированно отказывать новому при заполнении одними running entries.
   - Tests: exactly one confirm; legacy ghost removed; nested one block; known-message restart edits not sends; unknown-send restart sends no duplicate and records tombstone; old registry loads; retry cap; reply lookup checks parent+thread+message; bounded registry.

4. **Actor integration (`crates/cctg/src/hub/slots.rs`).**
   - Добавить options `correlate_for=60s`, `recheck_after=1s`; candidates/indexes/reports и bounded body coordinator.
   - В `on_hook`: nested Stop сохраняет bounded answer durably; SubagentStart/Stop создают/обновляют candidate; Handback сохраняется только для валидного `agent_id` и не может перейти к записи другой parent session; SessionEnd закрывает prompts и blocks. Ни SubagentStop, ни nested Stop не вызывают TASK-022 send благодаря существующему top-level/current gate — закрепить тестом.
   - Index/body file reads запускать через `spawn_blocking` вне actor, с one-read-per-session/key и глобальным concurrency cap. Stale generation result игнорировать.
   - В `pump` выдавать не более свободного block-job budget; Done освобождает budget и позволяет следующему pending job. Telegram stall не должен мешать ingress/hooks и не должен увеличивать очередь сверх константы.
   - `on_topic_message` добавляет `target_agent` только внутри existing explicit-reply branch и после `live_agent`; routing всегда использует parent connection.
   - Long final text: сначала durable block update, document — через существующую bounded message queue; при overflow preview остаётся корректным, document failure не создаёт повторный block.
   - Очищать indexes/candidates/reports/body requests по ended/pruned parent; nested answer больше не хранить в `nested_answers`.

5. **Acceptance/integration tests в `slots.rs`.**
   - Три explicit Agent correlations → одна topic и ровно три blocks; empty internal stop, every-Bash-style stop, typed `--agent` start/stop с файлами без correlation → ноль ghost blocks.
   - Stop до parent result, result позже; expiry, затем stop reopens window; report / finished brief / lagging file-last fallback; duplicate stops завершаются out of order, но report/newest generation остаётся итогом.
   - Known nested run: zero extra topics, ровно один block через повторный start, last answer только в edit; `NestedUnknownParent` — zero topic/block/channel route.
   - Reply to running и finished subagent block получает валидный `target_agent` только у live parent; reply на nested/foreign/old block — без него; nested agent получает ноль frames.
   - Restart: topics не создаются повторно; confirmed block с message id только edit; ended running block получает deterministic lost text; unknown first-send state не resend и получает tombstone; legacy registry ghost не оживает.
   - Stall stress: больше лимита candidates/reports/index/body requests/block jobs; actor продолжает принимать hook/inbound, queue sizes не превышают constants. Huge description и body не вызывают Bot API over-limit send.
   - Privacy integration test: новые warn/debug/info не содержат hook text, report, transcript path, Telegram user id или secret.

6. **Проверка implementer.**
   - Применить исправленный patch к текущему HEAD и проверить hashes только после внесения перечисленных изменений.
   - Использовать один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, один cargo за раз, `-j 1`; сначала targeted `cargo test -p cctg --lib hub::subagents`, registry/slots filters и `cargo test -p transcript --test subagent`.
   - Затем один `cargo test -j 1 --workspace --no-fail-fast`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`; удалить target dir.
   - Не вызывать Telegram API, не читать `.env`/`device.env`, не запускать interactive Claude.

Acceptance map: AC1 — three-explicit integration; AC2 — internal/typed-uncorrelated + legacy migration; AC3 — fallback and duplicate/out-of-order body tests; AC4 — nested + unknown-parent tests; AC5 — narrow parent routing tests; AC6 — known-message restart, unknown-send tombstone, no topic/block duplicates; AC7 — workspace/fmt/clippy run.

## 5. Risk areas

- **Неустранимая ambiguity первого Bot API send.** При падении после отправки и до ответа HTTP Bot API не позволяет узнать `message_id` или безопасно дедуплицировать retry. Принята at-most-once политика: возможен потерянный block, но не duplicate; это должно быть явно принято как уточнение AC6.
- **Remote devices.** По решению orchestrator parent/subagent transcripts читаются локально, поэтому remote candidates не скоррелируются до будущего file-fetch через agent. Nested blocks работают без файлов.
- **Correlation window/eviction.** Parent transcript, отставший более чем на 60 s, или экстремальный burst сверх bounded index/candidate limits может скрыть реального агента. Это контролируемый отказ без ghost; counters/логи не должны содержать content/path.
- **Very old finished blocks.** Durable cap требует eviction; reply на эвиктированный старый block больше не получает `target_agent`. Новые/running blocks вытесняются последними или не принимаются вовсе.
- **Flood budget.** Каждый block требует metered send и обычно edit. Ограничение dispatch защищает память, но увеличивает latency под Telegram throttling.
- **Large files.** 256 MiB parent scan и 64 MiB body read остаются дорогими; concurrency cap обязателен. Нельзя читать файлы на actor thread.
- **Report before restart.** Handback report остаётся memory-only и может потеряться при рестарте до stop; тогда используется transcript/last-message fallback. Persisting report расширило бы registry приватным объёмным текстом и в эту задачу не входит.
- **Registry compatibility.** Новые поля только defaulted при `VERSION=1`; migration должна удалять лишь однозначно legacy empty-header subagent records, не валидные новые blocks.

# TASK-017 PLAN V2: буфер мёртвого слота и Resume affordance

## 1. Review notes

### Обязательный disconfirmation

Проверен контрпример: сообщение принято hub-to-agent очередью и уже дошло до agent, после чего hub завершился до durable-save снимка, из которого сообщение удалено. Контрпример подтвердился в reference implementation: `scratch/planner/ws/crates/cctg/src/hub/slots.rs:1107-1123` делает `try_send`, затем сразу `pop_front`, а снимок только асинхронно публикуется saver-у в `pump` (`:2694-2698`). В `HubMsg::Inbound` нет delivery id/ack, agent не дедуплицирует inbound по Telegram `message_id`. Значит после аварии в этом окне возможна повторная доставка.

Это не блокирует задачу, потому что `OPEN_DECISIONS.md` явно оставляет agent-side dedup за рамками TASK-017 и принимает at-least-once на аварийной границе. Но исходный план нельзя формулировать как crash-safe exactly-once. Гарантия этой задачи: порядок и однократная передача в обычном in-process ходе; сохранённый, ещё не переданный буфер восстанавливается один раз; авария между hand-off и fsync может дать дубль. Надёжное устранение окна потребует отдельного wire capability и idempotent consumer. Это соответствует стандартной практике: transactional outbox даёт at-least-once и требует дедупликации consumer-а ([Microsoft Transactional Outbox](https://learn.microsoft.com/en-us/samples/azure-samples/cosmos-db-design-patterns/transactional-outbox/)).

### Недочёты исходного плана и reference implementation

1. **Resume не появляется при самом переходе в dead.** `offer_resume` в reference пропускает слот с пустым `buffer.messages` (`slots.rs:1208-1213`). Кнопка появляется лишь после первого текста. TASK_FINAL связывает её с dead-состоянием темы, а не с наличием буфера. Нужен тест `SessionEnd -> Resume` без входящих сообщений.

2. **Resume callback не привязан к показанному сообщению и dead-периоду.** `press` передаёт только разобранный session id в `press_resume` (`slots.rs:2132-2135`); `CallbackInput.message_id` игнорируется. Поэтому вручную сформированный `resume:<known-session>` и старая кнопка из предыдущего dead-периода той же сессии могут поставить `resume_asked` в новом периоде. Это слабее уже принятого TASK-014 правила, где callback разрешается по `message_id + id`.

3. **Удаление клавиатуры не гарантируется.** `drop_resume_button` отправляет edit как `Work::Callback`; ошибка только пишется в debug (`slots.rs:2400-2403`). Повтора нет, хотя acceptance требует убрать keyboard в конце периода, а существующие permission edits уже имеют `Due/InFlight/Failed/Done`, retry tick, предел попыток и считают `message is not modified` успехом.

4. **План смешивает dead-период с периодом непустого буфера.** `Buffer::close` сбрасывает одновременно overflow mark, Resume note и `resume_asked` только после полного flush. Dead-период на самом деле начинается на `SessionEnd` и заканчивается, когда новая живая top-level сессия становится текущей в слоте; ожидание её agent connection — отдельное состояние `NoChannel`. Из-за смешения исходный код не может показать кнопку в пустом dead-слоте и неточно определяет момент устаревания callback.

5. **`flush_all()` выполняет лишнюю работу на каждом событии actor-а.** `Slots::run` вызывает `pump()` после каждого agent/hook/control/done/timer event (`crates/cctg/src/hub/slots.rs:437-455`), а reference `pump` каждый раз сканирует все слоты и пытается выгрузить до 50 сообщений каждого. Это не await и не нарушает запрет на Telegram I/O внутри Slots, но работа не ограничена числом слотов и может ухудшать fairness actor-а. Достаточны адресные flush на message, SessionStart/`/clear`, agent registration/rebind и периодический retry scan.

6. **Обычный live-path создаёт ненужную запись реестра.** Reference всегда делает `push -> try_send -> pop`, ставит `registry.dirty = true` и тем самым сериализует snapshot для каждого live message. Для нестриминговой живой сессии это write amplification и ненужное кратковременное хранение текста. Прямой fast path безопасен, если он используется только при пустой очереди слота; при непустом буфере новое сообщение сначала добавляется в хвост.

7. **Restart test доказывает только спокойный сценарий.** `kept_messages_survive_a_restart_and_go_out_once` сначала вручную сохраняет pending buffer, затем ждёт сохранения пустого buffer. Он не моделирует подтверждённое crash window. Название и acceptance mapping должны явно не обещать больше утверждённой orchestrator-ом семантики.

8. **Не хватает обязательных граничных тестов.** Нет: кнопки без buffered text; stale press после следующего dead-периода той же session; wrong/missing `message_id`; retry финального edit; callback от неразрешённого пользователя именно с Resume data; двух слотов одной папки, где оживает только правильный слот; fairness/отсутствия full-registry scan на обычном pump; явного top-level headless/no-agent случая.

9. **Изменения существующих тестов в основном обоснованы, но требуют усиления.** Удаление `OFFLINE_NOTICE` и переход к `QUEUED_NOTICE` одобрены в `OPEN_DECISIONS.md`; перевод queue-overflow теста на photos сохраняет проверку TASK-021 text-only throttling; изменение `resume:x` с пустого ответа на expired логично. Однако последняя правка не должна легализовать Resume callback без message/period binding.

10. **Артефакты целостны, но base-note устарела относительно task branch.** Все семь файлов `scratch/planner/ws` совпадают с `hashes.txt`, а `task017.patch` проходит `git apply --check`. При этом текущий HEAD — `bd9cf00`, а заявленная база reference — `7cdaa99`; между ними лежат только task artifacts, не production code. Реализацию следует переносить выборочно по PLAN_V2, а не копировать reference без исправлений.

Исследование Telegram подтверждает: `callback_data` ограничен 1–64 **байтами**, `answerCallbackQuery` допускает 0–200 символов, inline keyboard можно заменить явным пустым markup ([официальный Bot API](https://core.telegram.org/bots/api)). Tokio `try_send` означает лишь помещение значения в bounded channel, а не обработку downstream consumer-ом ([Tokio `mpsc::Sender`](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html)); это нужно отражать в терминах доставки.

## 2. Updated understanding

- Тема — долговечный slot `(host, folder, ordinal)`, не session. Буфер принадлежит слоту. Новая top-level session, resume прежней session и `/clear` могут стать следующим текущим владельцем слота и получить сообщения. Nested runs и subagents не становятся `current_session` и буфер не оживляют.
- `Registry::state` уже даёт `Dead` для слота без живой current session, topic mutation уже меняет icon на dead, а topic не закрывается. TASK-017 не добавляет close/reopen operations.
- Allowlist применяется в `updates::classify` до создания `CallbackInput` или `Inbound`; в этих структурах user id уже отсутствует. `Parked` должен хранить только Telegram message/thread/reply ids и text.
- Живая top-level session без bound agent тоже должна буферизоваться с одним `QUEUED_NOTICE` за период недоступности — это утверждённое решение orchestrator-а. Resume при этом не показывается, потому что session не dead.
- Agent, зарегистрировавшийся до SessionStart, ждёт в `pending`; после SessionStart binding выполняется в `on_hook`. `/clear` переиспользует channel process через pid rebind. Поэтому flush должен запускаться после фактического binding, а не только после hook-а.
- Bounded agent queue имеет capacity 64, buffer — 50. `try_send(Ok)` сохраняет FIFO в этой очереди, но не доказывает socket write или приём Claude Code. TASK-017 не меняет wire protocol согласно `OPEN_DECISIONS.md`.
- `registry.json` version остаётся 1: новые поля `#[serde(default)]`, пустое состояние не сериализуется. `after_restart` очищает transient connection/busy state, но обязан сохранить buffer, Resume lifecycle и marks.
- Update offset сохраняется до обработки batch. Поэтому уже существующее окно «offset durable, buffer snapshot ещё нет» может потерять только что пришедший update при crash; TASK-017 его не расширяет и не решает.

## 3. Revised approach

### Slot buffer и live fast path

Добавить чистый `hub/buffer.rs` с `VecDeque<Parked>`, cap 50 и durable marks. В `on_topic_message`:

1. General/non-slot остаются silent; non-text получает существующий throttled `TEXT_ONLY_NOTICE` и не попадает в buffer.
2. Если `buffer.messages` пуст, есть live top-level bound agent и `try_send` успешен — выполнить существующие receipt/reaction/log действия напрямую. Не менять registry, кроме уже необходимого stream receipt.
3. Иначе положить message в хвост buffer, выкинуть oldest при 51-м, затем попытаться flush только этого slot. Наличие старого buffer запрещает direct-send нового сообщения, поэтому FIFO не нарушается.
4. При full/closed agent queue оставить front на месте. Повторять при следующем message этого slot, успешном bind/rebind и retry tick.

### Dead period и Resume lifecycle

Resume state хранится durable внутри slot buffer state, но логически отделён от `messages`. Dead period начинается, когда current top-level session становится ended, и заканчивается, когда любая живая top-level session становится current в этом slot.

- На первом reconcile dead slot с topic/current session создать один `ResumeNote` даже при пустом message buffer, записать его в snapshot до/в том же actor turn, затем поставить один `Op::Send`.
- Callback data сделать `resume:<period_token>`, где token — сохранённый случайный `u64` в hex. Это короче 64 байт, не пересекается с `allow:`/`deny:` и не зависит от длины session id. `ResumeNote` хранит token, ended session, optional Telegram message id, `resume_asked` и состояние финального edit. TASK-019 затем читает persisted note/session и `resume_asked`, как решено orchestrator-ом.
- Для at-most-one offer note/tombstone создаётся до отправки и не удаляется при неясном результате. 429 обрабатывает Scheduler. Не повторять первичный send после ambiguous HTTP outcome, чтобы не создать вторую кнопку; это явно указать как residual.
- Resume press разрешать только для allowlisted callback, валидного token, активного note и совпадающего `CallbackInput.message_id` (когда send result дал id). Missing/wrong id, token старого периода и неизвестный token отвечают `ANSWER_EXPIRED`. Повторный press активной кнопки идемпотентно оставляет `resume_asked = true` и возвращает понятный `ANSWER_UNAVAILABLE`. Если slot уже ожил — `ANSWER_ALIVE`, без изменения intent.
- На окончании dead period немедленно сделать note closing и запланировать edit с явным `{ "inline_keyboard": [] }`; текст не должен утверждать, что buffer уже доставлен, если agent ещё не bound. Использовать нейтральное `RESUMED_TEXT`, например «Слот снова активен; кнопка Resume больше не нужна.»
- Финальный edit вести durable состояниями `Due/InFlight/Failed/Done`, повторять на retry tick до небольшого существующего-style cap (5), считать `message is not modified` применённым. Если send result с message id пришёл уже после revival, сразу перевести note в `Due`. После success/give-up note можно удалить; live slot не создаст новую.

### Адресный flush и fairness

Не вызывать `flush_all` в каждом `pump`.

- `on_topic_message` пробует только свой slot.
- Agent registration/rebind пробует slot связанной current session.
- SessionStart/new session/`/clear` делает lifecycle reconcile нужного slot; фактический message flush происходит сразу, если agent уже bound, иначе после bind.
- Retry tick сканирует buffered slots и пробует их по порядку/с сохранённого round-robin cursor. Один slot содержит максимум 50, поэтому можно ограничить одну tick-итерацию фиксированным числом slots, чтобы actor продолжал принимать ingress. Следующий tick продолжает с cursor.
- Все Telegram sends/edits по-прежнему только `hand_off` в dispatch/Scheduler; Slots нигде не await-ит Telegram и не await-ит bounded scheduler queue.

### Delivery semantics

- Внутри непрерывного процесса сообщения удаляются из slot buffer только после успешного `try_send` в правильную bound connection; FIFO и отсутствие повторной постановки проверяются тестами.
- После normal durable recovery pending messages отправляются и затем durable очищаются; следующий normal restart их не повторяет.
- Crash после queue hand-off, но до durable clear, даёт at-least-once и возможный duplicate; link loss после queue acceptance может дать loss. Это утверждённые residuals TASK-017, не маскируемые словом «ровно один раз».

## 4. Revised steps

1. **Добавить `crates/cctg/src/hub/buffer.rs` и экспорт в `hub/mod.rs`.**
   - `MAX_BUFFERED = 50`, `Parked { message_id, thread_id, text, reply_to }`, `Buffer { messages, overflow_told, queued_told, resume }`.
   - `ResumeNote` содержит period token, ended session, `message_id`, `resume_asked`, send/edit state и число edit failures.
   - `push` возвращает факт drop-oldest; helpers для `resume:<hex-token>`, parse, keyboard, texts. Считать байты callback data, не Unicode code points.
   - Unit tests: exact 50 cap/order; defaults/round-trip; 64-byte bound; несовместимость с allow/deny; malformed token; lifecycle state transitions; answer texts ≤200 chars.

2. **Расширить `registry::Slot` совместимо с version 1.**
   - Поле buffer — `#[serde(default, skip_serializing_if = "Buffer::is_idle")]`; transient edit-in-flight после restart переводится обратно в due/failed, но durable intent/messages сохраняются.
   - Инициализировать поле во всех `Slot` literals; дать actor-у узкий mutable accessor, caller выставляет `dirty`.
   - Registry tests: старый JSON без buffer загружается; idle field отсутствует; messages, marks, active Resume и closing edit round-trip; `after_restart` сохраняет их; corrupt/foreign/duplicate-topic поведение неизменно.

3. **Переделать `Slots::on_topic_message` без write amplification.**
   - Вынести построение `HubMsg::Inbound` и post-send receipt/reaction в helpers.
   - Direct fast path только при пустой message queue; иначе park then flush.
   - На overflow отправлять один `OVERFLOW_NOTICE` за период backlog/deadness; mark ставить только когда notice принят dispatch cap-ом. `QUEUED_NOTICE` — один за период live-without-agent, как утверждено в OPEN_DECISIONS.
   - Не логировать text, user id, path или title; допустимы ordinal, short session id, counts и фиксированные строки.

4. **Добавить адресный flush и revival wiring.**
   - `flush_slot(slot)` проверяет именно live top-level current session и newest bound connection, отправляет front-to-back до первого `try_send` failure.
   - Вызывать его после сообщения этого slot, agent bind/rebind, SessionStart если agent уже существует, `/clear` transfer и retry tick.
   - Nested SessionStart известной top-level session, nested new run, SubagentStart/Stop и headless top-level без agent не вызывают успешный flush.
   - Добавить bounded round-robin retry scan вместо `flush_all()` на каждом `pump`; `pump` остаётся synchronous и не ждёт Telegram.

5. **Реализовать Resume как lifecycle dead slot, а не непустого buffer.**
   - Reconcile после hook/registry transition предлагает кнопку один раз сразу после dead icon/topic availability, даже если messages пуст.
   - New session taking slot, same-session resume и `/clear` закрывают старый Resume note; отсутствие agent не оставляет активную кнопку dead session.
   - Первичный send учитывает общий `MAX_QUEUED_MESSAGES`; late send completion корректно либо записывает message id, либо сразу запускает closing edit.
   - Финальный edit с пустой keyboard сохраняется и retry-ится как permission final edit; `message is not modified` — success.

6. **Ужесточить callback routing.**
   - Parse Resume до permission parser только по отдельному prefix, затем найти active note по token и проверить `message_id`.
   - Stale/foreign/missing-message callbacks не меняют registry и отвечают `ANSWER_EXPIRED`; active callback ставит durable `resume_asked`; already-live отвечает `ANSWER_ALIVE`.
   - Не менять общий allowlist gate в `updates.rs`; добавить regression test с `resume:<token>` от stranger, доказывающий `Ignored(NotAllowed)` и отсутствие `Control::Callback`.

7. **Обновить существующие тесты только там, где это требует утверждённое поведение.**
   - TASK-021: заменить offline-loss ожидания на buffer/`QUEUED_NOTICE`; сохранить отдельные throttling tests text-only notices и silent General/non-slot behavior.
   - TASK-014: foreign permission callback остаётся без verdict/edit; Resume-shaped callback получает expired только после строгой token/message проверки. Не ослаблять проверку stale permission presses.
   - Queue-cap test продолжает заполнять Telegram backlog photos/text-only notices; отдельно тестировать, что buffer warning не обходит cap.
   - `message_logs.rs` проверяет отсутствие private text и user id в logs, наличие только text/message metadata в parsed `registry.json` и отсутствие любых `from`/`user_id` полей, а не только substring одного числа.

8. **Добавить actor tests, непосредственно покрывающие acceptance.**
   - `SessionEnd` без messages: dead icon, topic не закрыт, ровно один Resume send; repeated pump/tick/restart не создаёт второй.
   - 51-й text вытесняет ровно oldest; 52+ продолжают sliding window; warning один за dead period и re-arm после следующего dead period.
   - Full agent queue: остаток остаётся FIFO; новое сообщение не обгоняет старое; следующий targeted retry продолжает с front.
   - Revival table: same-session resume, new session, `/clear`; agent-before-hook и agent-after-hook; buffer уходит один раз в правильном порядке.
   - Два concurrent slots одной folder: dead slot #1 с buffer и live slot #2; revival/reuse #1 не посылает данные agent-у #2.
   - Nested resume, nested run, subagent и top-level headless/no-agent не опустошают buffer.
   - Resume callbacks: active/repeated, wrong message id, missing id, unknown token, old token после нового dead period той же session, press после revival; keyboard edit success, `not modified`, transient failure/retry и retry cap.
   - Fairness: событие одного slot не сканирует/flush-ит все slots; retry cursor даёт progress двум buffered slots; stalled Telegram не блокирует ingress.
   - Live fast path: нестриминговое успешное сообщение не сериализует buffer/snapshot; buffered path остаётся durable.

9. **Добавить/скорректировать real-link integration test `tests/buffer_e2e.rs`.**
   - Real `serve_agents`, Slots, Scheduler, fake Telegram transport и raw TCP peer.
   - Dead slot получает три текста; затем его занимает resumed или новая top-level session; agent регистрируется позже; получает `registered`, затем три `Inbound` с ids/text в FIFO и больше ничего в пределах timeout.
   - Проверить отдельным restart phase: pending buffer предварительно durable, новый Slots load не дублирует Resume offer, после нормального flush durable buffer пуст и следующий load ничего не выдаёт.
   - Не называть этот тест доказательством exactly-once при crash между hand-off и save.

10. **Верификация.**
    - Проверить patch относительно текущего production tree, не копировать весь `scratch/planner/ws`.
    - Использовать один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo process за раз; удалить target dir после проверки.
    - Итерационно: buffer/registry unit filters, затем Slots filters для cap/revival/callback/fairness, затем `--test buffer_e2e`, `message_logs`, `overflow_logs`, существующие permission/TASK-021 regressions.
    - Финально: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` при доступной памяти.
    - Acceptance matrix в итоговой проверке: каждый пункт TASK_FINAL должен ссылаться минимум на один actor test; restart/persistence и TCP ingress — также на integration test.

## 5. Risk areas

- **Crash semantics:** queue hand-off и registry save не атомарны. Возможны duplicate после crash или loss при разрыве link после `try_send`; устранение требует additive capability + agent ack/dedup и отдельной задачи.
- **Update offset gap:** offset сохраняется раньше actor state; crash до buffer snapshot может потерять update. Это существующая TASK-009 семантика.
- **Ambiguous Resume send:** повтор после HTTP timeout может создать две кнопки, поэтому первичный send остаётся at-most-once с durable tombstone. Редкий lost answer может оставить неизвестный `message_id`, который нельзя отредактировать.
- **Permanent edit failure:** keyboard может остаться после retry cap; stale token/message validation обязана сделать её безопасной и вернуть `ANSWER_EXPIRED`/`ANSWER_ALIVE` без intent mutation.
- **Registry growth/privacy:** до 50 текстов на каждый давно неиспользуемый slot остаются в gitignored local `registry.json`; slots пока не pruned. Никаких Telegram user ids рядом с ними нет.
- **No-channel session:** agent process может быть зарегистрирован, хотя Claude Code silently drops channel notifications. Hub не умеет доказать потребление; это известное ограничение channel domain.
- **Actor fairness:** retry scan должен быть bounded и round-robin; нельзя заменять его await-ом или Telegram call внутри Slots.
- **Semantic handoff:** reply на старый subagent block, доставленный новой session того же slot, сохраняет Telegram reply metadata, но `target_agent` вычисляется заново и не должен указывать subagent прежней session.
- **Callback collisions/security:** token lookup плюс Telegram message id и общий allowlist gate обязательны; одного prefix/session id недостаточно. `resume:` остаётся непересекающимся с `allow:`/`deny:`.
- **Two-slot routing:** buffer никогда не переезжает между ordinal slots; новая session получает только buffer занятого ею свободного slot.

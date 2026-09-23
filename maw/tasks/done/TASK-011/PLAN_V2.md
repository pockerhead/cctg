# PLAN V2 — TASK-011: hub, слотовый реестр и жизненный цикл тем

## 1. Review notes

Референс собирается: локально проверены `cargo test --workspace --offline` (221 passed, 1 ignored), `cargo clippy --workspace --all-targets --offline -- -D warnings`; `task011.patch` применим к текущему HEAD. Но переносить его байт-в-байт нельзя:

1. **Ingress всё ещё зависит от Telegram.** `hub/slots.rs::pump` делает `outbox.submit(op).await`. `Outbox` — bounded `mpsc` на 1024; scheduler не вычитывает его во время `transport.execute().await` и `retry_after`. Заполненная очередь останавливает чтение AgentEvent/hooks и возвращает риск 5-секундного timeout из TASK-010.
2. **Неизвестная session ошибочно объявляется top-level.** Через 10 s `Slots::on_tick` вызывает `Registry::adopt`; неизвестные `Stop`/`UserPromptSubmit` тоже создают top-level session. Nested run с задержанным/потерянным SessionStart получает тему, вопреки nested=no-topic.
3. **Stale self-parent обработан против hooks contract.** `Registry::nesting` и тест `a_stale_parent_pid_pointing_at_itself_is_top_level` превращают parent PID, разрешившийся в ту же session, в top-level. Контракт требует идти к следующему предку; текущий wire этого не позволяет, поэтому безопасный результат — NestedUnknownParent без slot.
4. **`TOPIC_ID_INVALID` способен создать две replacement-темы.** `topic_work` одновременно выпускает Separator и Edit одного slot; separator не ставит busy. После первого invalid запускается replacement Create; поздний второй `topic_invalid` безусловно снимает busy до проверки thread id и позволяет второй Create. Существующий тест не покрывает invalid во время in-flight replacement.
5. **Separator теряется до подтверждения.** `pending_separator.take()` выполняется при enqueue. Ошибка, закрытый oneshot или crash после сохранения оставляют смену session без separator.
6. **Icon validation небезопасно деградирует.** При ошибке `getForumTopicIconStickers` остаются непроверенные константы. Bot API требует ids из этого метода; lookup error должен останавливать startup, а отсутствующие preferred ids — заменяться ids из реально полученного набора. См. [Telegram Bot API](https://core.telegram.org/bots/api).
7. **ai-title ограничен первыми 4 MiB без контрактного основания.** Следует потоково искать первую ai-title запись до EOF с общим 256 MiB safety cap.
8. **Open questions уже закрыты.** `OPEN_DECISIONS.md`: title остаётся ≤128 UTF-16 units; общий host helper — TASK-012/013; NoChannel expiry — TASK-017; device cwd canonicalization — TASK-013.

Проверенный disconfirmation case: после смерти A почти одновременно стартуют B и C в одной папке. Контрпример не подтвердился: Slots единолично владеет Registry, а allocate+occupy синхронны; B занимает старый slot, C получает #2. Точного regression-теста этого варианта всё равно нет.

Подтверждено и сохраняется: slot = `(host, folder_key, ordinal)`; reuse/resume/`/clear`; lexical folder_key; сохранение `[host]` и `#N`; отсутствие closeForumTopic; SlotLocator; startup grace; temp+file-fsync+rename; single-owner actor; удаление forum_topic_edited; отсутствие новых crates. Актуальный Rust использует на Windows `MoveFileExW`/новый rename API; round-trip test проверяет замену существующего файла ([std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html)).

## 2. Updated understanding

- `wire.rs` уже несёт `SessionStart { source, claude_pid, parent_claude_pid }`. Some(parent) означает найденного внешнего claude-предка; отсутствие PID в hub не доказывает top-level.
- Два ingress channel в `hub/ingress.rs` bounded (256). Consumer должен делать только синхронные registry mutations и неблокирующий handoff.
- `hub/scheduler.rs` — единственная точка Telegram I/O/429, но enqueue в bounded Outbox может ждать. Нужен отдельный handoff worker.
- `hub/updates.rs` распознаёт forum_topic_edited до allowlist. Bot API разрешает удалять service messages с can_delete_messages, кроме сообщения создания topic ([deleteMessage](https://core.telegram.org/bots/api)).
- `Op` не имеет close-topic. SessionEnd меняет только state/icon.
- Registry хранит slots, current sessions, metadata, PID map и applied topic state. Connections, waiting/in-flight/retry — ephemeral.
- После restart topic bindings не пересоздаются. Re-register возвращает Alive; session без agent становится NoChannel после grace. Expiry относится к TASK-017.
- Topic name локально ограничивается 128 UTF-16 units по решению orchestrator; новый live probe не нужен.
- Достаточны существующие `tokio`, `serde`, `serde_json`, `thiserror`, `tracing`, `transcript`; manifests не меняются.

## 3. Revised approach

Сохранить чистый `hub/registry.rs` и single-owner actor `hub/slots.rs`, исправив state machine.

### Registry

- `folder_key`: снять extended/UNC prefix, привести separators к `/`, убрать trailing separators, case-fold drive/UNC. `folder_name` хранит первое display spelling.
- Allocation только для доказанной top-level session: собственный slot; прежний свободный slot resume; slot прежней session того же `(host, claude_pid)` для /clear; первый свободный ordinal; затем max+1.
- Nesting: no parent → TopLevel; известный чужой parent → Nested(parent); unknown или stale-self → NestedUnknownParent. Nested-виды не получают slot.
- Неизвестные agent registration, Stop и UserPromptSubmit не создают session/slot. Agent ждёт SessionStart либо остаётся без topic.
- Subagent с непустым agent_type ссылается на slot известного parent; пустой type игнорируется.
- Для slot допускается ровно одна in-flight topic operation. Приоритет: Create → persisted Separator → Edit. Separator ставит busy и удаляется только после success.
- Result несёт ожидаемые `(slot, topic_id, generation)`. Late/duplicate mismatch — полный no-op, включая busy. Invalid очищает binding один раз; replacement Create остаётся busy до ответа.
- Title truncation сохраняет host/#N; label — ai-title либо short id. Reader в spawn_blocking потоково ищет первое ai-title до EOF/256 MiB.
- Icon map строится после успешного catalog lookup: preferred ids, если есть; иначе различные детерминированные ids из sorted allowed set. Lookup error или менее четырёх ids — startup error без token/URL.
- Load fails closed на malformed/version/bad refs. Save: fixed temp, write, file sync_all, rename; torn temp не влияет на старый JSON.

### Non-blocking actor

- `Slots::run` не вызывает async Outbox submit на ingress path.
- В `Slots::new` создать внутренний unbounded dispatch channel. Actor синхронно передаёт `(job, Op)`; отдельный worker ждёт bounded enqueue и Telegram response, затем шлёт Done.
- `on_control` сделать sync. Без delete right — ноль Delete и один startup warning. Runtime failures: один warn, затем debug.
- Saturation test: stalled fake transport и >1024 jobs не мешают actor принять следующий hook/agent event в короткий local deadline.

### Integration

- `mod.rs`: registry load до topic mutations, строгий icon catalog, scheduler, dispatch worker и Slots вместо drain_ingress.
- `SlotLocator`: current session по thread id; General/unknown/prefix сохраняют ProjectsDir fallback; пустой transcript path → typed NoTranscript.
- forum_topic_edited известного slot направляется в Slots и удаляется при наличии права; forum_topic_created не удаляется.

## 4. Revised steps

### Step 0 — guardrails

1. Проверить `git status --short -- Cargo.toml Cargo.lock crates`; не затирать пользовательские изменения.
2. Не читать `.env`, не вызывать Telegram, не трогать `~/.claude`.
3. Один cargo process за раз; CARGO_TARGET_DIR под `%TEMP%`.
4. Reference patch использовать как материал, не применять целиком. Manifests/lock не менять.

### Step 1 — `crates/cctg/src/hub/registry.rs`

1. Добавить Slot/Session/Subagent/Registry/Icons/Store types и чистые helpers.
2. Реализовать allocation/reuse/resume/`/clear` и folder normalization.
3. Реализовать conservative nesting; удалить agent-only и unknown-hook adoption.
4. Реализовать Alive/Dead/Waiting/NoChannel и allowed icon mapping.
5. Сделать последовательную per-slot state machine с persisted separator и generation-checked results.
6. Реализовать load validation, restart reset только ephemeral state, pruning и atomic save.
7. Unit tests:
   - A dead, затем queued B+C: B reuse без Create и с одним separator; C ровно один Create #2;
   - concurrent #2/#3, resume, /clear, Windows/UNC/POSIX spellings;
   - nested known/unknown/stale-self/nested-of-nested и subagents: zero topics;
   - unknown Stop/UserPrompt/agent-only: zero topics;
   - title ≤128 UTF-16 с emoji/Cyrillic, host/#N сохранены, ai-title заменяет id;
   - все четыре icons входят в supplied set; missing preferred получает allowed fallback;
   - torn temp, foreign version, round-trip;
   - failed separator остаётся pending; separator и edit не in-flight одновременно;
   - два late invalid во время replacement дают суммарно один Create и не снимают busy.

### Step 2 — `crates/cctg/src/hub/slots.rs`

1. Реализовать actor с agent/hook/control/done/timer inputs.
2. Удалить adoption по hook_wait; pending agent связывается только после SessionStart и удаляется при disconnect.
3. Добавить dispatch worker; в actor не оставлять await на enqueue/Telegram path.
4. Оставить startup grace только для edits, retry desired state, late-agent binding, потоковый title scan.
5. Реализовать service delete и warn-once.
6. Actor tests:
   - hook-only → NoChannel; late agent → тот же slot Alive, без Create;
   - agent-before-hook не создаёт topic до hook, после — ровно один;
   - agent without hook и unknown Stop/UserPrompt не создают topic;
   - restart сохраняет topics; reconnect reconciliation не создаёт topics;
   - SessionEnd даёт Dead edit и ни одной close operation;
   - stalled Telegram + saturated scheduler не задерживает ingress;
   - TOPIC_NOT_MODIFIED applied; invalid race → одна replacement;
   - failed separator доходит после retry без второго schedule;
   - known edited service удаляется; unknown thread/no right — no Delete.

### Step 3 — integration files

1. `hub/sessions.rs`: SlotLocator/TopicView, NoTranscript, прежний fallback.
2. `hub/commands.rs`: exhaustive безопасный message для NoTranscript.
3. `hub/mod.rs`: modules, registry load, strict icons, Slots/control/dispatch; удалить drain_ingress.
4. `crates/cctg/tests/slots_logs.rs`: отдельный real-tracing test с without_time; ровно один warning и отсутствие paths/title/user ids/secrets.

Ожидаются те же шесть project paths, что в reference patch. `scheduler.rs` и `wire.rs` менять не нужно: handoff и conservative nesting помещаются в slots/registry.

### Step 4 — verification

1. `cargo fmt --all -- --check`.
2. `cargo clippy --workspace --all-targets --offline -- -D warnings`.
3. `cargo test --workspace --offline`.
4. Пять последовательных прогонов registry/slots/sessions и отдельного slots_logs; cargo не параллелить.
5. `git diff --check`; status показывает только шесть ожидаемых paths.
6. Acceptance matrix связывает тесты с: dead reuse+one separator; live #2/#3; nested/subagent zero topics; no close+valid icons; title bound; torn save+one replacement; service deletion+warn once; hook-only/late agent; folder_key; full existing suite.

### Step 5 — commit

После зелёной матрицы один английский commit без generated/co-author trailers: `feat(hub): add durable slot registry and topic lifecycle (TASK-011)`.

## 5. Risk areas

- **Separator exactly-once.** Bot API sendMessage не имеет idempotency key: при потерянном ответе нельзя гарантировать отсутствие и дубля, и потери. State machine гарантирует один schedule для известных результатов; ambiguous failure policy документировать.
- **Unbounded handoff.** Он изолирует ingress, но может расти при outage. Registry jobs ограничены одним на slot; Delete events допустимо coalesce. Не возвращать backpressure в actor.
- **Unknown sessions.** Без agent-only adoption session с навсегда потерянным SessionStart не получит topic. Это безопаснее нарушения nested=no-topic; mid-session recovery требует доказанного signal в TASK-012/013.
- **PID reuse/restart.** Persisted PID map нужен живым parents, но stale mapping не должен создавать slot. Self/unknown → NestedUnknownParent.
- **Durability.** File fsync+rename защищают от torn content; power-loss durability каталога вне критерия.
- **Icon lookup.** Strict startup failure задержит hub при временной ошибке Telegram, зато непроверенный id не отправляется.
- **Service correlation.** Bot API не связывает forum_topic_edited с конкретным edit call. Удаление любого такого service в managed slot соответствует текущему UX, но может удалить ручной rename; не добавлять хрупкий счётчик без требования.
- **Title semantics.** 128 UTF-16 units — закрытое решение orchestrator, не open question.
- **Filesystem identity.** Hub делает только lexical normalization; symlink/junction/8.3 остаются TASK-012/013.

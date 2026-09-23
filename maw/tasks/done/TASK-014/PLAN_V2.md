# TASK-014 PLAN V2: permission relay end to end

## 1. Review notes

### Проверенный опровергающий пример

До оценки плана был проверен конкретный контрпример: сессия показывает permission prompt, затем получает `SessionEnd`, после чего пользователь нажимает старую кнопку Allow. Контрпример **подтвердился**.

- `OPEN_DECISIONS.md` требует на `SessionEnd` запрашивающей сессии закрыть все её открытые prompts, отредактировать сообщение в `Сессия завершилась` и убрать кнопки.
- В reference implementation `scratch/planner/ws/crates/cctg/src/hub/slots.rs:429-431` на `SessionEnd` удаляется только состояние transcript scan. Prompt book не меняется.
- Тест `a_prompt_goes_to_the_slot_of_its_session_after_the_slot_moved_on` (`slots.rs:1849-1896`) закрепляет противоположное поведение: prompt сессии A остаётся активным после её `SessionEnd` и отправляется уже после занятия слота сессией B.
- Разделы Approach и Risk исходного плана также прямо говорят не закрывать prompt на `SessionEnd`.

Следовательно, исходный план противоречит принятому решению оркестратора и должен быть изменён. Результат проверки сохранён в `scratch/reviewer1_disconfirmation.md`.

### Ошибки и пропуски исходного плана

1. **`try_send` не подтверждает доставку verdict.** В reference `slots.rs:739-756` prompt помечается решённым, waiting снимается и кнопки удаляются сразу после успешного `mpsc::Sender::try_send`. Это подтверждает только попадание в локальную очередь hub. `hub/ingress.rs:186-226` ещё должен записать сообщение в TCP, а `agent.rs:12` прямо фиксирует, что сообщение, запись которого оборвалась, теряется. При обрыве между `try_send` и чтением агентом UI уже говорит «решено», но терминальный диалог остаётся открытым навсегда. Нужен delivery id и ack от agent link.

2. **PID fallback недостаточно ограничен и может выбрать другую сессию.** `scratch/planner/ws/.../slots.rs:776-785` ищет новый conn только по `host + claude_pid`, не проверяя `Conn.session == Prompt.session`; даже исходный conn принимается без проверки, что он не был rebound после `/clear`. При reuse PID или rebind старый verdict с пятибуквенным id может попасть агенту другой сессии. Короткий id не доказывает, что это тот же prompt. Fallback обязан также совпадать по сохранённой session.

3. **Состояние waiting рассинхронизируется.** `slots.rs:648` выставляет waiting до того, как `Prompts::open` действительно принял prompt. При ошибке Telegram `Done::Permission` удаляет prompt (`872-880`), но не снимает waiting. Обратно, после решения одного prompt `slots.rs:760` безусловно снимает waiting, даже если у сессии есть второй открытый prompt. Нужна одна функция, вычисляющая waiting из всех незавершённых prompts этой session, и вызовы на каждом переходе состояния.

4. **Удаление кнопок не гарантировано.** После первого успешного `try_send` reference делает единственный `Op::Edit`. `Done::Callback` лишь пишет debug при ошибке; retry отсутствует. Значит временная ошибка Telegram оставляет активные кнопки под уже решённым prompt. Final edit должен иметь отслеживаемую revision/state и повторяться через существующий retry tick до успеха; повторный callback всё равно не должен слать второй логический verdict.

5. **Bounded prompt book выбрасывает активный prompt, оставляя живые кнопки.** `permissions.rs:135-162` при полном book сначала выбрасывает decided prompt, а если их нет — самый старый undecided. Telegram-сообщение при этом не редактируется, waiting не пересчитывается. Нельзя молча evict активный или selected prompt: при отсутствии завершённой жертвы новый prompt следует не зеркалировать в Telegram, оставив терминальный dialog доступным.

6. **Первый ответ не представлен как устойчивое состояние.** В исходном плане до успешного `try_send` prompt остаётся полностью undecided, поэтому при offline/full queue следующий callback может выбрать противоположное действие. Нужно один раз зафиксировать выбранный `behavior` и стабильный `verdict_id`; повторные нажатия могут только повторить доставку того же логического verdict.

7. **Тесты reference зелёные, но не покрывают найденные дефекты.** Независимая копия `scratch/reviewer1_ws` прошла `cargo test --workspace --no-fail-fast -j 1`: cctg lib 250 passed / 1 ignored, остальные integration/transcript/doc tests также прошли. Однако нет теста закрытия prompts на `SessionEnd`, ack после link drop, retry финального edit или корректного waiting при двух prompts/ошибке отправки. Существующий moved-slot test, наоборот, закрепляет уже отменённое решение.

8. **Reference основан на старом commit, но остаётся применимым.** Текущий repo HEAD — `1f50bb9`, а reference создан от `c002525`. Последующие commits не меняли `crates/cctg/src`, `crates/cctg/tests` или manifests; `git apply --check` для `task014.patch` на текущем HEAD проходит. Хэши пяти файлов reference совпадают с `hashes.txt`. Тем не менее patch нельзя применять как окончательную реализацию: перечисленные lifecycle/wire исправления должны войти сразу.

### Что в исходном плане подтверждено и сохраняется

- Allowlist gate уже правильно расположен в `hub/updates.rs:130-149`: callback постороннего пользователя не превращается в `Control`, downstream не получает user id и не отвечает деталями.
- `message_id` является правильным scope внутри единственного настроенного Telegram chat. Совпадение `message_id + request_id` различает одинаковые пятибуквенные ids у разных sessions; callback без доступного message id безопасно устаревает.
- Scheduler уже реализует нужную дисциплину: permission send обгоняет сообщения других topics, но не более раннюю работу собственного topic (`scheduler.rs:329-409`, тесты `permission_never_overtakes_its_own_topic` и `permission_overtakes_other_topics_only`). Новый scheduler или отдельная Telegram task не нужны.
- Slots actor уже не ждёт Telegram: unbounded hand-off идёт в `dispatch_loop`, который один ждёт место в Scheduler. Эту границу надо сохранить.
- Reference не добавляет crates. Текущий согласованный набор (`tokio`, `serde`, `serde_json`, `reqwest`, `anyhow`, `tracing`, `thiserror`, `subtle`, `transcript`) достаточен; manifests и `Cargo.lock` менять не нужно.

### Проверка Bot API

Подход сверен с официальным [Telegram Bot API](https://core.telegram.org/bots/api):

- `InlineKeyboardButton.callback_data` — 1–64 **bytes**, поэтому `allow:abcde` (11 bytes) и `deny:abcde` (10 bytes) корректны;
- `answerCallbackQuery.text` — 0–200 characters; все фиксированные ответы значительно короче;
- `CallbackQuery.message` имеет тип `MaybeInaccessibleMessage`, поэтому callback без пригодного message id должен быть безопасным stale case;
- `editMessageText.text` — 1–4096 characters after entity parsing; проект консервативно использует `transcript::telegram_len` в UTF-16 units;
- `reply_markup` у `editMessageText` — optional inline keyboard. Для снятия кнопок остаётся явный `{"inline_keyboard": []}` и обязательная live-проверка после merge, как решил оркестратор. Реальный Telegram API во время review не вызывался.

## 2. Updated understanding

На текущем HEAD permission relay частично реализован только на agent/channel стороне:

- `channel.rs:234-273` валидирует пятибуквенный request id, ограничивает каждое поле 32 KiB, подавляет недавний повтор request и посылает `AgentMsg::PermissionRequest` в hub.
- `channel.rs:181-201` превращает `HubMsg::PermissionVerdict` в `notifications/claude/channel/permission`. Claude Code применяет verdict только к своему pending id; ответ терминала hub не видит.
- `wire.rs:126-189` содержит request и verdict без delivery acknowledgement. Комментарий `wire.rs:13-14` требует bump протокола при новом message type.
- `hub/ingress.rs` даёт каждому соединению уникальный `conn`, bounded `to_agent` queue и отдельный writer с timeout. Сейчас inbound принимает только `Reply` и `PermissionRequest`.
- `hub/slots.rs` — единственный владелец registry/connections. В базовом коде permission request только выставляет waiting; Telegram callback в `hub/mod.rs:74` пока логируется и теряется.
- `registry.apply_hook` корректно игнорирует `SessionEnd` от отличающегося nested-resume pid и только для принятого SessionEnd выставляет `ended=true`, `waiting=false`. Поэтому закрывать prompts надо не по сырому variant, а только после подтверждённого перехода session в ended.
- `Scheduler` уже имеет permission lane, FIFO собственного topic, edit coalescing, 429 retry и bounded fairness. `Op::Send { permission: true }` должен оставаться единственным способом отправить prompt.
- `updates::classify` выполняет allowlist до создания `CallbackInput`; inaccessible/inline callback без message id не может выбрать prompt.

Prompt должен быть привязан одновременно к:

- session на момент получения request;
- её slot (через сохранённую `SessionEntry.slot`, а не текущую session slot-а);
- исходному agent process (`host`, `claude_pid`) и session;
- Telegram `message_id` после успешного send;
- отдельному случайному `verdict_id`, который не попадает в `callback_data` и нужен только для надёжной hub↔agent доставки.

Принятые решения оркестратора уже закрывают все прежние open questions:

- UI wording reference сохраняется;
- открытые prompts закрываются на принятом `SessionEnd` с текстом `Сессия завершилась` и без кнопок;
- снятие кнопок пустой inline keyboard проверяется live после merge.

## 3. Revised approach

### Prompt state machine

В `hub/permissions.rs` ввести явные состояния вместо одного `decided: Option<Behavior>`:

1. `Open` — кнопки активны, behavior ещё не выбран.
2. `Selected { behavior, verdict_id, in_flight_conn }` — первый allowlisted callback уже навсегда выбрал behavior; verdict либо ждёт подходящего conn, либо отправлен и ждёт ack. Повторный callback не может изменить behavior и не создаёт новый logical verdict.
3. `Decided { behavior }` — agent подтвердил получение данного `verdict_id`; desired Telegram rendering — исходный bounded prompt плюс planner mark, empty keyboard.
4. `Closed` — принятый `SessionEnd` закрыл ещё не решённый prompt; desired rendering — ровно `Сессия завершилась`, empty keyboard, любые поздние verdict/ack игнорируются.

Отдельно хранить lifecycle Telegram send/edit: not sent, send in flight, shown(message_id), desired render revision и edit in flight. Это нужно для гонок `SessionEnd` с `sendMessage`/`editMessageText` и для retry кнопок без повторной отправки prompt.

Book остаётся bounded до 256 записей. Evict разрешён только для полностью финализированного `Decided`/`Closed` prompt, whose desired edit Telegram уже принял. Если все 256 записей активны, новый request не показывается в Telegram и waiting не меняется из-за этого request; терминальный dialog остаётся fallback. Активные кнопки молча не забываются.

### Надёжная доставка verdict

Добавить внутренний delivery handshake:

- `HubMsg::PermissionVerdict` получает случайный `verdict_id: u64`;
- новый `AgentMsg::PermissionVerdictAck { verdict_id }` подтверждает, что agent link принял verdict и поставил соответствующий `LinkEvent` в очередь channel loop;
- поскольку добавляется новый wire message type, `wire::VERSION` повышается с 1 до 2 и обновляются `KINDS`/round-trip tests;
- cache последних 256 `verdict_id` живёт в agent link task через reconnect. Первый экземпляр ставится в channel event queue ровно один раз; повтор того же id после потерянного ack не создаёт вторую Claude notification, но ack посылается снова;
- cache обновляется только после успешной постановки `LinkEvent`. Ack пишется тем же единственным TCP writer-ом. Ошибка записи приводит к reconnect, но не забывает dedupe id;
- hub переводит `Selected` в `Decided` только по совпадающему ack от `in_flight_conn`. На disconnect очищается лишь `in_flight_conn`; выбранные behavior и verdict id сохраняются и отправляются заново после reconnect.

Таким образом, TCP frame может быть повторён после неоднозначного обрыва, но Claude получает ровно один logical verdict. Link drop между hub `try_send` и agent receive больше не превращает prompt в ложное «решено».

Connection selection обязана проверять session:

- исходный conn подходит только если ещё существует и `Conn.session == Prompt.session`;
- fallback — newest conn с совпадающими `host`, `claude_pid` **и** `session`;
- `SessionEnd` закрывает prompt и запрещает дальнейшие retries;
- отсутствие pid разрешает только исходный всё ещё связанный с той же session conn.

### Telegram lifecycle и waiting

- Prompt send идёт через `Op::Send { permission: true }`, не считается в `MAX_QUEUED_MESSAGES`, но bounded `MAX_PROMPTS` ограничивает память.
- Topic определяется через `registry.sessions[prompt.session].slot`, а не reply routing текущей session. Topic separator, уже handed off для того же slot, остаётся раньше prompt благодаря существующему scheduler FIFO.
- Первый callback немедленно получает короткий `answerCallbackQuery`; final text/buttons меняются только после ack. Повтор до ack может подтолкнуть retry того же verdict id, но не создать другой verdict.
- Final decision/SessionEnd edit имеет revision. Ошибка edit оставляет desired rendering pending; retry выполняется на существующем slots retry tick без tight loop. `Outcome::Superseded` старой revision не завершает новую.
- На принятом `SessionEnd`: shown prompt редактируется в точный текст `Сессия завершилась` с empty keyboard; unsent prompt удаляется и никогда не показывается; send-in-flight prompt становится `Closed`, и если `sendMessage` позднее вернёт message id, немедленно получает closing edit. Поздний callback/ack не посылает verdict.
- `waiting` пересчитывается одной функцией как наличие `Open` или `Selected` prompts данной живой session. Пересчёт выполняется после open/refusal, send failure, selection, ack, disconnect/reconnect и accepted SessionEnd. Это предотвращает stuck icon после failed send и преждевременное снятие при двух prompts.
- Claude не сообщает hub о terminal answer. Поэтому prompt, отвеченный только в терминале, может оставаться известным hub до последующего наблюдаемого hook/SessionEnd; это ограничение нельзя честно устранить в TASK-014. Оно не оправдывает рассинхронизацию на событиях, которые hub действительно видит.

### Text, buttons, privacy

- `callback_data` остаётся строго `allow:<id>` / `deny:<id>`; никаких session, conn, Telegram ids или hub token в кнопке.
- Callback валиден только при совпадении parsed behavior/id и `message_id -> prompt`. Два sessions с одинаковым request id остаются независимыми.
- Prompt — plain text без parse mode. `prompt_text` резервирует место под самый длинный decision suffix и режется через UTF-16-aware `registry::cut`; `Сессия завершилась` заведомо короче лимита.
- В логи не попадают request id, verdict id, Telegram user id, tool name, description, input preview, callback data, secret или private paths. Допустимы только fixed text, `conn`, short session и behavior.
- Stranger callback остаётся полностью на существующем allowlist gate: без verdict, edit, callback answer и раскрытия деталей.

## 4. Revised steps

Все пути относительны repo root. Reference patch использовать как основу структуры, но не применять без изменений.

1. **Wire contract — `crates/cctg/src/wire.rs`.**

   - Повысить `VERSION` до 2, потому что добавляется новый message type.
   - Добавить обязательный `verdict_id: u64` в `HubMsg::PermissionVerdict`.
   - Добавить `AgentMsg::PermissionVerdictAck { verdict_id }` и обновить `AgentMsg::KINDS`/`HubMsg::KINDS`.
   - Обновить все constructors/matches в source и tests.
   - Добавить round-trip tests для verdict/ack, version rejection и fixed malformed errors без отражения содержимого.
   - Check: `cargo test -p cctg --lib wire::`.

2. **Agent-side at-most-once forwarding across reconnect — `crates/cctg/src/agent.rs` и минимальные match changes в `channel.rs`.**

   - Держать bounded recent verdict-id cache в долгоживущем `run`, а не внутри одного TCP `serve`, чтобы cache переживал reconnect.
   - При первом verdict после успешного `events.send(LinkEvent::Message(...))` запомнить id и записать ack. При повторе того же id не создавать второй LinkEvent, но снова записать ack.
   - Ошибка ack write завершает текущий link и запускает обычный reconnect; id остаётся в cache.
   - `channel::Server` игнорирует internal `verdict_id` при формировании Claude notification; JSON-RPC остаётся `{request_id, behavior}`.
   - Tests: verdict проходит в channel ровно один раз; потерянный ack + reconnect + повтор того же id дают один LinkEvent и второй ack; другой id проходит отдельно; malformed request id не подтверждается как применимый verdict.
   - Check: `cargo test -p cctg --lib agent:: channel::`.

3. **Pure prompt model — новый `crates/cctg/src/hub/permissions.rs`; export из `hub/mod.rs`; `registry::cut` сделать `pub(crate)`.**

   - Перенести из reference parsing, keyboard, fixed UI strings и UTF-16 bounded formatting.
   - Реализовать `Open / Selected / Decided / Closed`, Telegram send/edit state, stable verdict id, message index и bounded order.
   - `open` подавляет дубликат незавершённого `(session, request_id)`, evict-ит только полностью rendered final prompt и возвращает явный `Full`, если все entries активны.
   - Добавить queries/transitions: unsent, by_message, by_verdict_id, select once, mark in-flight/disconnected/acked, close_session, delivered, desired edit revision, complete/fail edit, has_pending_for_session.
   - Unit tests: обе callback strings round-trip и ≤64 bytes; invalid/extra data отвергаются; huge astral preview даёт prompt и оба decision texts ≤4096 UTF-16 units; exact `Сессия завершилась`; одинаковый id двух sessions не конфликтует; second selection cannot flip behavior; active entries не evict-ятся; indexes очищаются при safe eviction.
   - Check: `cargo test -p cctg --lib hub::permissions`.

4. **Route callbacks — `crates/cctg/src/hub/mod.rs`.**

   - Добавить `Control::Callback(CallbackInput)` в slots API.
   - Вместо текущего `info!("inbound button press")` передавать allowlisted callback в control channel; при остановленном actor логировать только fixed warning.
   - Расширить routing test: command идёт command worker-у, text и callback — slots actor-у, stranger по-прежнему не доходит из `updates::classify`.

5. **Ingress accepts delivery ack — `crates/cctg/src/hub/ingress.rs`.**

   - После register разрешить `PermissionVerdictAck` наряду с `Reply` и `PermissionRequest`; handshake messages после регистрации по-прежнему игнорируются/предупреждаются.
   - Не логировать verdict id или request contents.
   - Добавить link test: hub посылает verdict, получает ack event; split lines и concurrent outbound traffic остаются корректными.

6. **Slots actor production wiring — `crates/cctg/src/hub/slots.rs`.**

   - Внести `Prompts`, callback control, permission send, prompt edit и ack handling в `Slots`, `Work`, `Done`, `dispatch_loop`.
   - `on_permission_request`: проверить conn и id; snapshot `session/host/pid`; сначала принять prompt в bounded book, затем синхронизировать waiting. Full/duplicate не создают Telegram job.
   - `send_prompts`: искать topic через сохранённую session slot; помечать send-in-flight до hand-off; использовать только `Op::Send { permission: true }`; не расходовать reply cap.
   - `on_callback`: всегда быстро enqueue `AnswerCallback`; валидный первый callback атомарно выбирает behavior/verdict id; последующие не меняют выбор. По `message_id + request_id` исключить cross-session routing.
   - `send_selected_verdicts`: выбирать только conn той же session; `try_send` меняет лишь `in_flight_conn`, не `Decided`. На register/tick повторять выбранный verdict без нового id, если ack отсутствует.
   - `on_agent(PermissionVerdictAck)`: принимать только matching id от matching in-flight conn; переводить в Decided, синхронизировать waiting и ставить versioned final edit.
   - `Disconnected`: очистить in-flight только у verdicts этого conn и синхронизировать icon; следующий подходящий reconnect повторяет тот же logical verdict.
   - `on_hook`: сравнить состояние session до/после `registry.apply_hook`; только действительно принятый переход в ended вызывает `close_session`. Не закрывать prompt на проигнорированный nested-resume SessionEnd. Обработать shown/unsent/send-in-flight cases из Approach.
   - `on_done`: record message id; если state уже Closed, сразу поставить closing edit. Failed prompt send удаляет невидимый prompt и пересчитывает waiting. Prompt-edit success завершает только совпадающую revision; error оставляет её для timed retry.
   - `pump`: сохранить порядок topic work → permission prompt sends → selected verdict retries → pending final edits → snapshot. Ни один новый путь не ждёт Telegram или scheduler queue внутри actor.
   - Не менять алгоритм scheduler: его permission/FIFO/429 semantics уже правильны.

7. **Slots and scheduler-facing tests — `slots.rs` tests.**

   Покрыть каждый acceptance criterion и перечисленные adversarial cases:

   - `callback_data` обеих кнопок exact и ≤64 bytes; huge emoji preview, initial/allow/deny rendering ≤4096 UTF-16 units.
   - Два живых sessions выдают один и тот же пятибуквенный id; callback message B даёт verdict только agent B. Отдельно проверить stale message, mismatched id, inaccessible message и foreign button.
   - Первый Allow, повторный Allow и поздний Deny до/после ack создают один logical verdict и одну Claude-facing delivery; behavior не меняется.
   - Смоделировать потерю link после hub queueing, но до ack: disconnect сбрасывает только in-flight, reconnect той же session получает тот же verdict id, agent dedupe даёт одну channel notification, ack завершает prompt.
   - Conn, rebound в другую session после `/clear`, и новый процесс с reused pid не подходят для старого prompt.
   - 256 reply sends заполняют actor cap в topic A; permission prompt topic B оказывается не позже позиции 1. Существующий scheduler test отдельно подтверждает, что prompt не обгоняет более раннюю работу своего topic.
   - Decision ack создаёт edit с planner mark и exact empty keyboard. Первый edit failure оставляет pending revision, tick повторяет edit, success завершает её; повторные кнопки не слют verdict.
   - Два prompts одной session: решение первого сохраняет waiting icon, решение второго возвращает alive. Failed prompt send последнего не оставляет waiting; disconnect даёт no-channel, reconnect восстанавливает waiting; SessionEnd даёт dead.
   - Accepted SessionEnd: shown prompt редактируется в exact `Сессия завершилась` без кнопок; unsent prompt не появляется; send-in-flight prompt после late send result немедленно закрывается. Любой поздний callback/ack не шлёт verdict. Ignored nested-resume SessionEnd prompt не закрывает.
   - Slot reuse acceptance: prompt сначала показывается в topic slot-а A; после accepted SessionEnd и занятия slot-а B именно старое message id получает closing edit, а не verdict B. Это одновременно подтверждает session-owned slot routing и новое решение SessionEnd.
   - Stalled Telegram transport не блокирует обработку следующего agent request, hook, callback или ack slots actor-ом.

8. **Privacy/allowlist integration test — новый `crates/cctg/tests/permission_logs.rs`.**

   - Использовать отдельный test binary, `.without_time()` и fast `BucketConfig` по существующим log tests.
   - Прогнать request с уникальными private sentinel tool/description/preview/secret-like strings, allowlisted callback, duplicate callback, stranger callback, disconnect/retry/ack и SessionEnd close.
   - Assert: stranger классифицирован `Ignored::NotAllowed`; только allowlisted path создаёт logical verdict; logs содержат ожидаемые fixed lifecycle lines, но не содержат request id, verdict id, callback data, tool/description/preview, secret-like sentinel, Telegram sender ids или private paths.

9. **Полная автоматическая проверка.**

   - Работать с одним `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo process за раз; удалить target после завершения.
   - Выполнить:
     - `cargo fmt --all --check`
     - `cargo clippy --workspace --all-targets -- -D warnings`
     - `cargo test --workspace --no-fail-fast -j 1`
   - Проверить `git diff --check` и отсутствие изменений в dependency manifests/lock.

10. **Live acceptance после merge, выполняет оркестратор, не implementer.**

    - По `docs/poc.md` запустить hidden-console interactive Claude с temporary `--mcp-config` и `--settings`; не регистрировать leak-prone user-scope probe.
    - Вызвать реальный tool permission: в правильном slot topic появляется bounded prompt, ❓ icon и Allow/Deny.
    - Нажать Allow: debug log Claude показывает matched pending permission, terminal dialog закрывается, tool продолжает работу; после ack Telegram message имеет planner mark, кнопок нет, icon возвращается в ⚡️.
    - Повторный старый callback не создаёт второй tool verdict и отвечает «Уже решено».
    - Отдельно завершить session с открытым prompt: message становится ровно `Сессия завершилась`, кнопки исчезают, позднее нажатие не влияет ни на новую session slot-а, ни на terminal.
    - Подтвердить фактическое удаление кнопок с `{"inline_keyboard": []}`. Если live API это отвергнет, зафиксировать отдельный follow-up на узкий `editMessageReplyMarkup`; не скрывать провал acceptance.

## 5. Risk areas

- **Ack означает приём agent link, не подтверждение Claude Code.** Agent ставит `LinkEvent` в локальную очередь до ack, поэтому обычный TCP link drop verdict не теряет. Крах всего agent process после ack, но до stdout notification остаётся узким неустранимым окном без ответа от Claude Code. SessionEnd закроет UI; live test проверяет фактический happy path.
- **Terminal answer невидим hub.** Claude Code не сообщает, что dialog выиграл терминал. Hub может хранить такой prompt до следующего наблюдаемого lifecycle event. Нельзя использовать это как основание переслать противоположный verdict или раскрыть данные; поздний Telegram verdict Claude проигнорирует, если id уже не pending.
- **Callback может обогнать `Done::Permission`.** Telegram theoretically может доставить update до того, как actor обработал message id из scheduler completion. Такой callback получает stale answer и не создаёт verdict; повтор работает. Не добавлять сложный orphan-callback buffer без воспроизводимого сбоя.
- **Final edit может постоянно падать.** Retry устраняет transient failure и не допускает второй verdict, но permanent 400 оставит видимые кнопки. Логировать fixed warning не чаще одного на prompt/retry episode; live acceptance обязан проверить empty keyboard.
- **Hub restart теряет in-memory prompts/acks.** Старые кнопки после restart становятся stale и не могут отправить verdict. Persisting sensitive prompt text в `registry.json` не входит в TASK-014 и нарушило бы surgical/privacy scope.
- **Prompt capacity.** При 256 одновременно активных prompts новый request остаётся только в terminal; активные Telegram buttons не evict-ятся. Это безопаснее ложного UI и ограничивает память.
- **Ordering.** Permission send может обгонять только другие topics. Более ранний separator/message/document собственного topic должен остаться впереди; не обходить Scheduler прямым Bot API call.
- **SessionEnd races.** Closing transition должен быть revisioned: late prompt-send result, late verdict ack и superseded decision edit не имеют права вернуть `Closed` в `Decided` или снова показать кнопки.
- **Privacy.** `tool_name`, `description` и `input_preview` могут содержать команды, файлы и secrets. Они допустимы только в allowlisted Telegram prompt и в памяти; ни один error/debug path, serde error или test fixture не должен их печатать.
- **Protocol rollout.** Wire version 2 требует одновременного обновления hub и agents. Существующий version rejection делает несовпадение явным и безопасным; rolling mixed-version compatibility в scope не добавлять.

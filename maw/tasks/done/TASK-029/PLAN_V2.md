# TASK-029 PLAN V2: закреплённый статус и безопасное прерывание

## 1. Review notes

### Проверенный контрпример

Перед оценкой был зафиксирован контрпример: слот находится в фазе `❓ Ждёт разрешения`, пользователь подтверждает ⏹, агент пишет один Esc в открытую permission-панель. В этом состоянии Esc отвечает на диалог (по смыслу — deny), но не обязан завершать agentic turn. Контрпример **подтвердился**:

- `hub/status.rs::render` показывает кнопку interrupt и для `Phase::Waiting`;
- `slots.rs::busy` считает `SessionEntry.waiting` достаточным условием, а `press_status` без отдельной ветки отправляет `ConsoleKey::Interrupt`;
- `keys::press` возвращает только успех `WriteConsoleInputW`, после чего `slots.rs::on_key_done` немедленно вызывает `activity.stop()`;
- e2e-тест подставляет `ConsoleKeyDone { ok: true }` и проверяет idle, но не открывает реальный permission prompt;
- живая проба `probe.esc_text.out.txt` проводилась только во время thinking, не во время permission prompt.

Следовательно, `ok=true` доказывает лишь запись двух `INPUT_RECORD` в буфер, не потребление Esc Claude Code и не окончание хода. В текущем варианте Telegram может показать `💤 Ждёт вас`, хотя Claude лишь отклонил permission и продолжил работу.

### Конкретные проблемы исходного плана и патча

1. **Удаляется чужое pin-сообщение.** `updates::classify` распознаёт service messages до allowlist и теряет `from.id`; тест намеренно принимает `pinned_message` и от бота, и от allowlisted администратора. Затем `slots::on_pinned` удаляет любое уведомление, если `pinned_message.message_id` совпадает со status message. Поэтому ручное закрепление пользователем нашего status message будет удалено как будто это действие бота. Bot API описывает `pinned_message` как поле обычного service `Message`, но не обещает, что автор всегда бот: [Telegram Bot API](https://core.telegram.org/bots/api#message).

2. **Ложное подтверждение interrupt.** `ConsoleKeyDone.ok` названо как результат прерывания, хотя реализация подтверждает только `WriteConsoleInputW`. Microsoft прямо определяет успех как число записанных событий; API кладёт их после уже ожидающих событий и не подтверждает обработку приложением: [WriteConsoleInput](https://learn.microsoft.com/en-us/windows/console/writeconsoleinput). Сообщение надо переименовать по смыслу (`ConsoleKeyWritten`) и не делать из него `Activity::stop()`.

3. **Нет безопасного поведения при permission prompt.** Риск описан как «ожидаемо», но он противоречит критерию остановки хода. Пока отдельная скрытая живая проба не докажет нужное поведение, interrupt-кнопка в фазе `Waiting` должна быть скрыта; старый callback должен отвечать, что сначала нужно решить permission, и не посылать клавишу.

4. **Поздний ответ способен попасть в следующий сеанс слота.** `KeyAsk` не хранит соединение, а `on_key_done` проверяет только session id. При `ok=false` он берёт текущий `topic_id` сохранённого слота и может отправить failure notice уже новому сеансу после rotation. Нужно привязать ask к `(slot, session, conn)`, повторно проверить live/current/bound connection и молча отбросить stale result.

5. **Статус инструмента застревает после отказа permission.** Официальная документация говорит, что permission denial стреляет `PreToolUse`, но не `PostToolUseFailure`; ручной deny также не покрывается `PermissionDenied` (тот только для auto mode): [PostToolUseFailure](https://code.claude.com/docs/en/hooks#posttoolusefailure), [PermissionDenied](https://code.claude.com/docs/en/hooks#permissiondenied). Патч оставляет такой tool id running до `Stop`. Уже существующий transcript stream несёт `StreamItem::Result { id, .. }`; его надо использовать как дополнительное идемпотентное завершение tool activity.

6. **После restart теряются цифры и статус сразу затирается.** `Activity.metrics` существует только в памяти. При перезапуске `shown` пуст, поэтому сохранённое Telegram-сообщение редактируется в `💤 Ждёт вас` без model/ctx/limits до следующего statusline trigger, который у уже идущей сессии может долго не прийти. Последние метрики нужно хранить в `registry.json` на уровне session; при смене session внутри slot они естественно меняются вместе с current session.

7. **Бюджет statusline уже нарушен измерением плана.** `hook_cost.out.txt` показывает median 170.4 ms и max 203.7 ms при недоступном hub, хотя критерий — 150 ms. Причина: сетевой timeout равен полному бюджету, плюс запуск процесса/парсинг. POST должен иметь меньший внутренний deadline (ориентир 75–100 ms) и идти параллельно пользовательской команде; проверять надо overhead относительно той же команды без cctg.

8. **Chaining не доказан end-to-end.** Есть unit-тесты выбора command/shell, но нет теста запуска реальной команды с теми же stdin и побайтового stdout. На ошибке запуска/timeout патч печатает собственную строку, хотя у пользователя с настроенным statusLine это меняет вид терминала. Если user command существует, нужно передавать ровно его stdout (включая пустой, ANSI и несколько строк); fallback cctg допустим только когда user command отсутствует. Также надо сохранить/явно перенести сопутствующие `statusLine` options (`padding`, `refreshInterval`, `hideVimModeIndicator`) в cctg `--settings`, иначе «строка не меняется» не выполнено. Официально эти поля влияют на поведение: [Claude Code statusline](https://code.claude.com/docs/en/statusline).

9. **Проценты валидируются неверно.** Код допускает `0..=1000`, тогда как документированные five-hour/seven-day percentages лежат в `0..=100`; невалидные значения должны отбрасываться, не показываться как `999%`: [statusline available data](https://code.claude.com/docs/en/statusline#available-data).

10. **Пробелы в acceptance coverage.** Три `status_e2e` теста не проверяют: callback от неallowlisted пользователя через настоящий update router; permission prompt; hub restart; reuse/rotation одного slot; delayed key result; agent without/with capability over stale connection; statusline byte-for-byte chaining и 150 ms budget; ConPTY/mintty failure. Наличие 604 passing tests подтверждено `workspace_test.txt`, но эти сценарии ими не доказаны.

11. **Неподтверждённый Ctrl+B раздувает scope.** Orchestrator уже решил оставить ⏬ выключенной до отдельной успешной пробы. Несмотря на это, patch добавляет `Background` во все слои, options, rendering и тесты. Это мёртвая feature branch внутри feature и нарушает правило surgical changes. В TASK-029 не добавлять Ctrl+B wire/API/UI вообще; вернуться к нему отдельной задачей после пробы.

12. **Размер описан неясно.** Patch действительно затрагивает 33 файла, но `git apply --numstat` даёт 3059 additions / 54 deletions; «4114 строк diff» — размер patch с заголовками/context, не объём изменённых строк. Основной лишний объём — 634 строки e2e и выключенная background-функция. Тесты нужны, но их следует разделить по инвариантам и убрать фоновые ветки.

13. **Неполный комплект входных артефактов.** Указанный `PREMISE_CHALLENGE.md` отсутствует. Это согласуется с `OPEN_DECISIONS.md`: premise stage был явно skipped. `scratch/planner/ws/` тоже отсутствует; реализация проверена восстановлением чистого HEAD и применением `task029.patch`. Эти отсутствия не надо превращать в новые open questions.

### Что в исходном подходе подтверждено и сохраняется

- Esc-проба для thinking успешна: stdio MCP child нашёл непосредственного Claude parent, сделал `FreeConsole` → `AttachConsole(parent pid)` → `CONIN$` → `WriteConsoleInputW`, ход отменился; `Stop` и interrupt note не пришли.
- Additive wire capability `Register.console_keys` с `#[serde(default)]` и без `VERSION` bump — правильный способ совместимости.
- Новые agent frames проходят через явный allowlist `hub/ingress.rs` и должны иметь real-TCP test.
- Slot actor не ждёт Telegram; status jobs идут через dispatch/scheduler. Edit coalescing и 5-second per-slot pace соответствуют существующей архитектуре.
- Одно persisted `StatusMessage { message_id, pinned }` на slot и reset при replacement topic — правильная модель.
- Async command hooks действительно не блокируют Claude Code; официальный контракт также предупреждает, что каждый fire создаёт отдельный процесс без deduplication: [async hooks](https://code.claude.com/docs/en/hooks#run-hooks-in-the-background). Поэтому bounded hub state и короткий собственный network timeout обязательны.
- Statusline JSON, nullable fields и частота из пробы соответствуют текущей официальной документации.
- `WriteConsoleInputW` остаётся допустимым best-effort Windows path, но Microsoft помечает API как legacy/no virtual-terminal equivalent; ConPTY/mintty должны иметь честный `written=false`, а не обещание поддержки: [Win32 note](https://learn.microsoft.com/en-us/windows/console/writeconsoleinput#remarks).

## 2. Updated understanding

- Status message принадлежит **slot**, а не session. `message_id` и факт pin сохраняются в `registry.json`; при смене current session тот же message редактируется, новый не создаётся. При replacement topic status сбрасывается вместе со старым topic id.
- Текущая фаза составляется из нескольких источников:
  - registry: live/dead/current top-level session и waiting permission;
  - `UserPromptSubmit` / `Stop` / `SessionEnd`;
  - узкие async `PreToolUse` + `PostToolUse` + `PostToolUseFailure` события;
  - transcript `StreamItem::Result` как authoritative cleanup tool id и `INTERRUPT_NOTE`, если он существует;
  - отправленный interrupt как отдельное состояние `InterruptSent`, но не как доказанный `Idle`.
- Tool activity и confirmation/key asks — только bounded runtime state. Последние statusline metrics — durable session state, чтобы переживать hub restart. Команды, tool input, Telegram user id и message text не логируются.
- Statusline wrapper получает один JSON stdin, параллельно делает короткий POST только с нужными данными (`session_id`, model, effort, ctx, 5h, 7d; common path fields оставить пустыми для этого event) и запускает пользовательскую statusLine-команду с теми же stdin/environment/shell semantics. Вывод configured command имеет приоритет и не заменяется fallback на её ошибке.
- Telegram `pinned_message` — service message с автором. Только событие от `getMe.id` текущего бота, указывающее на ожидаемый status `message_id`, разрешено удалить. Все чужие pins остаются без действий.
- Callback security остаётся на входе updates: сначала chat id, затем `from.id` allowlist. Slots actor всё равно проверяет status message id, current slot/session, live bound connection и capability — это defense in depth.
- `console_keys` означает «агент может попытаться писать console input», а не «любой Esc гарантированно завершает turn». Capability объявляется только на Windows при надёжно найденном собственном Claude parent. Клавиша всегда адресуется сохранённому pid этого parent; pid никогда не приходит от Telegram/hub.
- Разрешённые решения из `OPEN_DECISIONS.md` закрыты: 5-second pace, 10-second confirm, только user-level statusLine command, CLAUDE_CONFIG_DIR fallback line допустима, ⏬ не входит в этот patch, background task count пропускается как несвязанный scope.

## 3. Revised approach

### Status model

Добавить чистый `hub/status.rs` с `Phase::{Ended, Waiting, Tool, Thinking, InterruptSent, Idle}`, bounded `Activity` (до 16 running ids, до 64 recently-finished ids) и render-функцией. Приоритет фаз: Ended → Waiting → InterruptSent → newest tool → Thinking → Idle. Metrics брать из durable current session entry. Keyboard всегда передавать явно, включая empty keyboard.

Status создаётся только когда у live top-level current session уже принят separator. Затем один раз закрепляется. Edit разрешён не чаще раза в 5 s на slot; confirmation/expiry и исчезновение message — осознанные immediate exceptions, но всё равно не более одного status job на slot и с coalescing scheduler. Transient pin failure повторять по существующему retry cadence; permanent permission/4xx — warn once и оставить message незакреплённым, не штормить.

### Interrupt protocol

Оставить единственную allowlisted клавишу `Interrupt`; Ctrl+B полностью исключить. Callback flow:

1. `status:stop` на status message live/current/busy session с connected capable agent ставит confirmation `(slot, session, message_id, expires_at)` на 10 s и немедленно редактирует keyboard.
2. `status:confirm` действует только на ту же tuple и до deadline.
3. В `Waiting` кнопка не рендерится; forged/stale callback отвечает фиксированным «Сначала ответьте на запрос разрешения» и не отправляет Esc.
4. Hub посылает `HubMsg::ConsoleKey { key_id, Interrupt }` только connection, объявившему `console_keys`; pending ask хранит `(slot, session, conn, expires_at)`.
5. Agent worker последовательно вызывает Win32 helper только с pid собственного parent и отвечает `ConsoleKeyWritten { key_id, written }`.
6. `written=true` переводит status в `InterruptSent`, очищает running tool ids и убирает кнопку, но не утверждает `Idle`; `Stop`, stream interrupt note, новый prompt или SessionEnd дают следующий достоверный переход. `written=false` даёт rate-limited notice только если tuple всё ещё current/live и connection тот же.
7. Любой late answer после disconnect, `/clear`, SessionEnd или slot reuse молча отбрасывается.

### ToolStatus hook

Сохранить узкий async hook: `PreToolUse` → `ToolStart { id, safe brief line }`; `PostToolUse`/`PostToolUseFailure` → `ToolEnd { id }`. Отбрасывать subagent (`agent_id`), handback и malformed input до POST. Не добавлять background flag. В stream handler каждый `StreamItem::Result { id }` также вызывает `tool_end(id)`, что закрывает manual denial/validation и потерянный async Post. `Stop`/interrupt/session end очищают всё.

Hook timeout оставить коротким (≤300 ms) и async в settings. Hub хранит только bounded ids. Логи содержат только event kind и short session id; отдельные negative log tests подставляют уникальную строку команды и Telegram ids и доказывают их отсутствие.

### Statusline

`cctg statusline`:

- tolerant parse только нужных полей; percentages finite и `0..=100`, округление целого для показа;
- не пересылает cwd/transcript/tool text для `StatusLine`; event привязывается к уже известному session id;
- network attempt стартует параллельно chain с внутренним timeout 75–100 ms и общим измеряемым overhead ≤150 ms при down hub;
- user command читается на каждом вызове из user settings (`CLAUDE_CONFIG_DIR/settings.json`, иначе `~/.claude/settings.json`), guard предотвращает recursion;
- configured command получает byte-identical stdin и её stdout возвращается byte-for-byte даже при empty/nonzero; fallback cctg печатается только если command отсутствует или settings не читаются;
- cctg session settings сохраняют `padding`, `refreshInterval`, `hideVimModeIndicator` пользователя при подмене только `command`. Если статический `docs/hook-settings.json` не умеет это сделать, документированный setup должен генерировать merged temporary `--settings`; нельзя молча терять эти поля.

### Telegram ownership

Добавить sender id в `ServiceMessage` (без логирования значения) и передать `getMe.id` в update classification/routing. Для `Pinned` actor получает `{ service_message_id, pinned_message_id, sender_is_bot }`; delete только при `sender_is_bot && pinned_message_id == tracked_status_id` и ожидаемом/persisted bot pin. Pin чужого сообщения, чужое закрепление нашего status и service message без sender не удаляются.

## 4. Revised steps

1. **Сначала сократить reference patch до принятого scope.** Удалить `ConsoleKey::Background`, `background_button`, `status:bg`, Ctrl+B key encoding, background detection, UI/docs/tests. Не копировать `task029.patch` целиком; использовать его только как источник уже проверенных частей. Зафиксировать новый ожидаемый file list/numstat после изменений.

2. **Wire compatibility (`crates/cctg/src/wire.rs`).** Добавить `Register.console_keys: bool` с `#[serde(default)]` и skip-when-false; `ConsoleKey::Interrupt`; `HubMsg::ConsoleKey`; `AgentMsg::ConsoleKeyWritten { key_id, written }`; `HookEvent::{ToolStart, ToolEnd, StatusLine}` с optional/default fields. `VERSION` оставить 1. Обновить `Kinds` и round-trip/legacy JSON tests: старый register читается false, старый hub никогда не получает новый agent frame, новый hub не отправляет key без capability.

3. **Tool line helper (`transcript`).** Экспортировать минимальный `call_line` поверх существующего renderer, не дублировать формат. Проверить char-boundary/Telegram length. Не менять parser model сверх уже существующих `StreamItem::Result` ids.

4. **Async ToolStatus (`hook.rs`, `docs/hook-settings.json`).** Узкий input с `#[serde(default)]`; skip subagents/handback/missing ids; Pre создаёт capped line, Post/PostFailure завершает id. Все три command hook — `async: true`. Добавить unit tests порядка Post-before-Pre, bounded finished ids на hub side, malformed/huge input, отсутствие command text в logs. Обновить exact settings test.

5. **Statusline implementation (`statusline.rs`, `main.rs`, setup docs).** Реализовать concurrent chain+POST и strict overhead deadline. Не выводить input/errors с путями. Добавить integration test с временным user command, который копирует stdin hash и печатает multiline/ANSI bytes; варианты empty output, nonzero exit, recursion guard, missing command fallback, null/missing/range-invalid metrics. Добавить timing test с silent fake hook endpoint: p95/максимум с разумным CI slack доказывает, что cctg overhead не превышает 150 ms; измерение сравнивает ту же chain-команду без POST. Проверить перенос `padding`, `refreshInterval`, `hideVimModeIndicator` в generated cctg settings.

6. **Win32 helper (`keys.rs`, `agent.rs`).** Оставить `FreeConsole`/`AttachConsole(own_claude_pid)`/`CONIN$`/down+up/`FreeConsole` под global mutex; гарантировать cleanup на каждом early return и восстановить изменённый Ctrl handler state, а не оставлять процесс навсегда с ignored Ctrl+C. Non-Windows возвращает unsupported. Worker bounded (queue 4), one-at-a-time, `spawn_blocking`. Unit test проверяет только encoding; injected Presser test по real TCP проверяет exact pid/key routing и `ConsoleKeyWritten`, не заявляя turn completion. Process-tree test доказывает выбор непосредственного Claude Code parent и отбрасывание Desktop/другого процесса.

7. **Agent-link ingress (`hub/ingress.rs`, `channel.rs`).** Добавить explicit forwarding arm для `ConsoleKeyWritten`; `HubMsg::ConsoleKey` обрабатывать вне MCP/Claude JSON-RPC path. Real TCP e2e обязателен: capability true получает key, false не получает; message никогда не попадает в Claude stdout; stale duplicate connection не может подтвердить ask нового connection.

8. **Bot API/scheduler (`hub/api.rs`, `scheduler.rs`, `updates.rs`, `hub/mod.rs`).** Узкие поля `Message.pinned_message`, уже имеющийся `from`, `ChatMember.can_pin_messages`; `pinChatMessage(disable_notification=true)` через token-safe BotApi. `Op::Pin` — serialized unmetered mutation с 429 handling/fairness. Сохранить `getMe.id` и классифицировать pin с attribution. Unit tests: bot pin service routed; allowlisted и неallowlisted human pin не становится deletable bot notice; unknown/missing sender safe; no user id in logs.

9. **Durable registry (`hub/registry.rs`).** Добавить `Slot.status: Option<StatusMessage { message_id, pinned }>` и `SessionEntry.status_metrics` с serde defaults/skip-empty. При `StatusLine` обновлять только известную live top-level session; nested/ended/unknown игнорировать. При topic invalid сбрасывать status. При session rotation старый message id остаётся у slot, metrics выбираются по new current session. Load tests покрывают old registry, restart, duplicate-topic invariants и prune.

10. **Pure status state (`hub/status.rs`).** Реализовать bounded Activity, phase/render/callback constants, `InterruptSent`, 10-second confirm и explicit empty keyboard. Waiting никогда не предлагает interrupt. Unit tests: phase priority, Post-before-Pre, stream Result cleanup, restart metrics render, expiry, long Unicode line, exact callback data ≤64 bytes.

11. **Slots integration (`hub/slots.rs`).** Добавить per-slot `Shown`, one-in-flight `StatusJob`, paced deadlines, bounded `KeyAsk` с conn identity. Activity трекать только live top-level current session; `StreamItem::Result` завершает tool id, interrupt note/Stop очищает turn. На rotation очистить confirm/asks/activity старой session, но переиспользовать status message. `Slots` не await-ит Telegram: только `try_send` agent и dispatch jobs. Pin retry bounded; edit-not-found создаёт один replacement status и pin. Late/foreign `ConsoleKeyWritten` ничего не меняет и не уведомляет новый session.

12. **Status e2e разбить по инвариантам.** С real `serve_hooks`, real `serve_agents`, real update classification и fake Bot transport покрыть:
    - create → one pin → only bot-authored pin notice deleted;
    - human pins (allowlisted и чужой) untouched;
    - 5-second edit coalescing under ToolStatus/statusline storm;
    - restart from persisted registry: тот же message id, no second send/pin, metrics retained;
    - SessionEnd and reuse/rotation: same status message, dead has no button, stale callback/result never reaches successor;
    - two slots/two agents: key only own `(slot, session, conn)`;
    - callback from nonallowlisted user never reaches Slots/agent;
    - agent without capability, disconnected agent, dead session, foreign message id and expired confirmation send no key;
    - waiting permission shows no interrupt; forged callback sends no key; after permission closes and turn remains busy, a fresh confirm can send Esc;
    - `written=true` renders `InterruptSent`, not idle; independent Stop/note/session end completes transition;
    - edit-not-found replacement, transient pin retry and permanent no-pin permission path.

13. **Live-probe evidence, без новых пользовательских окон/API.** Сохранить существующую successful thinking probe как acceptance evidence. Не запускать interactive Claude в этой реализации. Документировать: classic hidden conhost confirmed; Windows Terminal/ConPTY expected because an underlying console session exists, but unverified; mintty/no-console produces `AttachConsole=false` → `written=false` and one bounded notice. Отдельная будущая approved probe нужна прежде, чем когда-либо включать interrupt во время permission prompt или Ctrl+B.

14. **Документация.** `docs/poc.md` перечисляет право Pin Messages, merged statusLine settings, async ToolStatus cost, 5 s pace/10 s confirm, Windows-only best effort и честные terminal limitations. Не упоминать ⏬ как доступную функцию. Объяснить, что status `Interrupt sent` не является terminal acknowledgement и что в permission phase надо нажать Allow/Deny.

15. **Лог/privacy gates.** Расширить isolated log tests уникальными sentinel command/path/user-id values для ToolStatus, StatusLine, callbacks и pin service; ни одно не появляется. Сетевые ошибки BotApi по-прежнему проходят `without_url`; serde errors остаются fixed-text. Проверить bounded maps/queues: activity 16/64, key asks 32 + 30 s, agent key queue 4, one status job per slot.

16. **Финальная проверка implementer-а.** Один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, строго один cargo process и `-j 1`: сначала узкие `cargo test -p cctg status`, `cargo test -p cctg --test status_e2e`, statusline/log/ingress filters; затем `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Target удалить после проверки. Не вызывать Telegram API, не читать `.env`/`device.env`, не запускать interactive Claude/окна.

### Acceptance mapping

- Emoji + model/effort/ctx/5h/7d и неизменённый terminal line: шаги 5, 9, 10, 12.
- Подтверждённый interrupt или честный отказ: существующая probe + шаги 6, 7, 13.
- Ровно один tracked status на slot, pin once, удаление только своего notice: шаги 8–12.
- ⏹ только live/current/own capable agent, 10 s confirmation, no cross-session: шаги 2, 6, 7, 10–12.
- Existing tests pass и wire VERSION=1: шаги 2 и 16.

## 5. Risk areas

- **Нет terminal acknowledgement.** Даже successful `WriteConsoleInputW` не подтверждает обработку. UI поэтому показывает `Interrupt sent`, пока не придёт независимый сигнал; это честнее ложного idle.
- **Permission dialog.** Один Esc может означать deny, а не stop. Кнопка скрыта в Waiting; включать её там нельзя без отдельной успешной живой пробы и определённого двухфазного протокола.
- **ConPTY/mintty/SSH.** Win32 API legacy и может не работать через некоторые transports. Failure остаётся `written=false`, без повторного key storm; capability — best effort.
- **Невозможная строгая exactly-once при потерянном HTTP response.** Telegram не даёт idempotency key для sendMessage. В нормальном/restart-after-persisted-state пути status один; crash между Telegram accept и durable message id теоретически может оставить orphan. Это надо явно документировать, не маскировать тестом fake transport.
- **Async hook process bursts.** Claude Code не дедуплицирует async hooks. Короткий self-timeout и bounded hub state ограничивают длительность/память, но очень высокая tool cadence всё ещё создаёт краткие процессы; измерить burst test и не добавлять третьи hooks без необходимости.
- **Statusline cancellation.** Claude Code отменяет предыдущий statusline run при новом update. POST at-most-once и metrics last-writer-wins; потеря одного update допустима, следующий восстановит. User command descendants на Windows могут пережить killed shell — не создавать дополнительные detached processes.
- **Metrics freshness.** Durable значения переживают hub restart, но остаются последними известными до нового Claude statusline invocation. Не вычислять и не выдумывать проценты в hub.
- **Pin permissions.** Без `can_pin_messages` статус можно отправлять/редактировать, но acceptance pin невозможен; startup даёт один warning без user id/token и docs требуют право.
- **Slot rotation races.** Все async completions обязаны повторно сверять topic/message/session/conn; ни notice, ни key result старой session не должен менять successor.
- **Scope regression.** Не возвращать отключённый Ctrl+B, background task count, project-level statusLine chaining или общий settings framework в TASK-029. Они требуют отдельных решений/проб.

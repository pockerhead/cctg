# TASK-001 — пересмотренный research report по декомпозиции MVP

Дата проверки: 2026-09-22. Локальная база фактов: `CLAUDE.md` и четыре нормативных доменных модуля в `maw/project-context/domains/`. Внешние утверждения ниже проверены по первичным источникам; где официальный источник не публикует числовой предел, это прямо отмечено.

## 1. Review notes

### Результат обязательной disconfirmation-проверки

Проверенный контрпример: «в разделе `CLAUDE.md` “Открытые вопросы” есть хотя бы один вопрос, не покрытый OQ-1…OQ-5 исходного отчёта». Контрпример **не подтвердился**: на диске ровно пять вопросов, и все пять были сопоставлены. Дальнейшая проверка, однако, нашла ошибки в выводах и декомпозиции.

### Что в исходном отчёте подтверждено

- В репозитории действительно нет `Cargo.toml`, Rust-кода и тестов; критерий о существующих тестах выполнен вакуозно по решению в `TASK_FINAL.md`.
- Официальная документация подтверждает `SubagentStop.agent_transcript_path`, асинхронное запаздывание transcript-файла и особое поведение `SubagentHandback` начиная с 2.1.271: отчёт нужно брать из `tool_input.message`, а не из `last_assistant_message` ([Hooks reference](https://code.claude.com/docs/en/hooks)).
- Подтверждены project-scope consent, user scope в `~/.claude.json`, необходимость development flag для custom channel и silent drop уведомлений, если сервер не загружен как channel ([MCP scopes](https://code.claude.com/docs/en/mcp), [Channels reference](https://code.claude.com/docs/en/channels-reference)).
- Подтверждены Telegram: имя темы 1–128 символов, текст сообщения 1–4096 символов после entity parsing, `callback_data` 1–64 байта и `retry_after` при flood control ([Bot API](https://core.telegram.org/bots/api), [Bot FAQ](https://core.telegram.org/bots/faq)).

### Ошибки и пробелы

1. **Crate layout чрезмерно раздроблен и противоречит нормативной ориентации.** Исходный план вводит шесть package-crates (`proto`, `hub`, `agent`, `hook`, `cctg`, `transcript`). Project context задаёт один binary `cctg` с тремя subcommands плюс отдельную чистую библиотеку `transcript`. Общие wire-типы не требуют отдельного crate: это небольшой внутренний модуль binary package. Исправленная схема — два workspace member.

2. **Hook transport выбран неверно.** `TASK-010/012` отправляют hooks через reconnecting TCP `proto`, тогда как нормативный hooks-модуль требует fire-and-forget HTTP POST с коротким timeout. Постоянный reconnecting client у короткоживущего hook-процесса и нарушает контракт, и раздувает scope.

3. **Нарушено правило “одна тема на session id”.** `TASK-011` предлагает при смене `session_id` на том же agent connection перепривязать старую тему. Это подходит модели «тема на процесс», но не cctg: resume того же id переиспользует тему, а новый id после `/clear` — новая сессия и новая тема. Nested run является отдельным исключением и прикрепляется к родителю без темы.

4. **Dead topic нельзя автоматически закрывать.** В закрытую тему нельзя нормально писать, а архитектура требует принимать сообщения для мёртвой сессии, буферить их и позднее предлагать resume. На `SessionEnd` меняется состояние/иконка; topic остаётся доступным.

5. **Вывод про state icon неверен.** Отсутствие изменяемого `icon_color` не отменяет state-by-icon: `editForumTopic` позволяет менять `icon_custom_emoji_id`, а `getForumTopicIconStickers` возвращает допустимые варианты ([Bot API, forum topics](https://core.telegram.org/bots/api#editforumtopic)). Поэтому перенос состояния в title не нужен и противоречит нормативному `[host] folder · ai-title`.

6. **Flood-control вывод слишком широкий.** Официальный FAQ публикует примерно 1 message/s в одном chat, 20 messages/minute в group и около 30 broadcasts/s, но не говорит, что `editMessageText` и `createForumTopic` расходуют те же числовые buckets. Числовой edit/topic-create rate limit не опубликован. Корректная стратегия: документированные limits применять к send operations; edits/topic mutations сериализовать и coalesce, а любой 429 обрабатывать по `retry_after`. Нельзя выдавать «30 API requests/s» за документированный предел.

7. **События hooks посчитаны неправильно.** `TASK-012` говорит «all five events», но нормативный список содержит шесть lifecycle events: `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`. Кроме того, для надёжного получения `SubagentHandback` нужен узкий `PreToolUse` или `PostToolUse` matcher на этот tool, как прямо рекомендует Hooks reference.

8. **Фильтр internal agents неполон.** Отбрасывание только пустого `agent_type` не удаляет internal events, когда основная session запущена с `--agent`: документация говорит, что тогда internal event получает имя main agent. Надёжнее коррелировать `agent_id` с явным `Agent` tool call/результатом родителя; пустой type можно отбрасывать сразу. Не подтверждённое различение следует проверить fixture/experiment, а не объявлять решённым.

9. **Граница чистой transcript-библиотеки размыта.** `TASK-007` формулирует чтение sibling `.meta.json` и выбор path внутри `transcript`. Нормативно библиотека не делает IO: она принимает строки; path и fallback выбирает hub. Аналогично `parse(&str) -> Vec<Turn>` следует сохранить, а `ai-title` читать отдельной pure-функцией.

10. **Telegram library recommendation недостаточно согласована с project law.** Сравнение трёх вариантов полезно, но bare `reqwest` выбран до измерения веса `teloxide`, хотя нормативная отправная точка — `teloxide`, fallback только если он окажется тяжёлым. `teloxide 0.17.0` покрывает Bot API 9.1 и содержит нужные MVP forum methods; master активен и добавил 9.2 ([changelog](https://github.com/teloxide/teloxide/blob/master/CHANGELOG.md)). `frankenstein 0.52` заявляет Bot API 10.3 и даёт typed reqwest client ([repository](https://github.com/ayrat555/frankenstein)), но не даёт достаточной причины менять утверждённую отправную точку. Все нужные MVP methods существуют задолго до 9.1.

11. **Soak-test неоднозначно считает темы.** Исходный scope запускает четыре одновременные сессии, одна из которых nested, плюс пятую во время остановленного hub; итог должен быть четыре top-level topics. Это надо написать явно, иначе критерий «четыре темы» выглядит как нарушение nested-инварианта.

12. **Описание текущего repo неточно.** Код действительно отсутствует, но repo не состоит из «трёх документов»: на диске есть MAW settings, agent context, task artifacts и domain docs. Существенный проверенный факт — отсутствие workspace/code, а не число Markdown-файлов.

## 2. Updated understanding

### Текущее состояние и целевая форма

Проект greenfield: нет Cargo workspace, исходников и тестов. Нормативная целевая структура:

```text
Cargo.toml                  # workspace, resolver = "2"
crates/cctg/                # единственный binary package
  src/main.rs
  src/hub/                  # Telegram, registry, routing, TCP + hook HTTP ingress
  src/agent/                # Channel MCP stdio + persistent TCP client
  src/hook/                 # stdin payload -> short HTTP POST -> exit 0
  src/wire.rs               # общие agent↔hub wire-типы, без отдельного crate
crates/transcript/          # pure parser/renderers, без IO/network/runtime
```

Так сохраняются один executable и одна отдельно тестируемая pure library. `hub`, `agent`, `hook` — subcommands и модули одного package, а не самостоятельные libraries.

### Открытые вопросы `CLAUDE.md`

| Вопрос | Статус и действие |
|---|---|
| Перезаписывает ли nested `claude -p` `CLAUDE_CODE_SESSION_ID`? | Документация это не гарантирует. `TASK-003` — эксперимент с hook stdin, выбранными `CLAUDE_*` и process ancestry. |
| Достаточен ли `SubagentStop.last_assistant_message`? | Нет как универсальный источник. Использовать captured `SubagentHandback.tool_input.message`; иначе brief из `agent_transcript_path`; `last_assistant_message` — последний fallback. Подтверждено [Hooks reference](https://code.claude.com/docs/en/hooks). |
| Как development channel ведёт себя при resume? | Не документировано. `TASK-004` проверяет fresh/resume/continue/missing flag наблюдением. |
| Нужен ли consent для `.mcp.json` в новой папке? | Project-scope server требует approval; user-scope доступен всем проектам. Отсутствие per-project consent для user scope дополнительно подтверждает `TASK-004` ([MCP docs](https://code.claude.com/docs/en/mcp)). |
| Лимиты массового создания topics? | Name 1–128; отдельный числовой create rate официально не опубликован. Сериализовать mutations, на 429 ждать `retry_after`; не выдумывать bucket. |

### Telegram constraints, необходимые MVP

- `createForumTopic.name`: 1–128 characters; bot нужен `can_manage_topics` ([createForumTopic](https://core.telegram.org/bots/api#createforumtopic)).
- `editForumTopic`: `name` 0–128 и изменяемый `icon_custom_emoji_id`; `icon_color` после создания не меняется ([editForumTopic](https://core.telegram.org/bots/api#editforumtopic)).
- `sendMessage.text`: 1–4096 characters after entity parsing; для MVP безопаснее plain text, чтобы не ломать Markdown entities ([sendMessage](https://core.telegram.org/bots/api#sendmessage)).
- `InlineKeyboardButton.callback_data`: 1–64 bytes ([InlineKeyboardButton](https://core.telegram.org/bots/api#inlinekeyboardbutton)).
- Published send guidance: около 1 message/s на chat, 20 messages/minute в group; excess приводит к 429. Отдельный числовой edit/topic-create limit не опубликован ([Bot FAQ](https://core.telegram.org/bots/faq)).
- При flood control `ResponseParameters.retry_after` задаёт число секунд до повтора ([ResponseParameters](https://core.telegram.org/bots/api#responseparameters)).

## 3. Revised approach

### Telegram client choice

Сравнение сохраняет три реальные альтернативы:

| Вариант | Сильная сторона | Цена/риск | Решение |
|---|---|---|---|
| `teloxide 0.17.0` | typed Bot API, long polling, forum methods, callback/update types, `Throttle`; активный master | released schema отстаёт от текущего Bot API, framework шире нужного | **Выбрать для MVP**, с минимальными features и измерением binary/RSS/build time в `TASK-008` |
| `frankenstein 0.52` | current typed surface (заявлена Bot API 10.3), optional reqwest client | меньше ecosystem/готовой orchestration; всё равно нужен свой scheduler/routing | Именованный fallback, если реально нужного метода нет в released teloxide |
| bare `reqwest` | полный контроль и минимальный API surface | вручную писать update/envelope/multipart/error models и поддерживать Bot API | Fallback только после измеренного доказательства, что typed clients неприемлемы |

Version lag `teloxide` не блокирует MVP: create/edit forum topics, long polling, callback queries, send/edit/document уже покрыты. Собственный outbound scheduler всё равно нужен для priority permission prompts, per-topic ordering, coalescing и общего `retry_after`; он оборачивает `teloxide`, не дублирует Bot API models.

### Архитектурный поток

1. `transcript` чисто разбирает строки и рендерит plain-text chunks/file payload decision.
2. `hub` принимает Telegram updates через `teloxide`, agent connections по newline JSON/TCP и hook events по HTTP POST.
3. `hook` — короткоживущий HTTP client без reconnect loop; ошибки hub никогда не ломают Claude Code.
4. `agent` — единственный persistent client: MCP stdio к Claude Code, TCP к hub, stdout только JSON-RPC.
5. Registry создаёт topic только для top-level session id. Resume того же id переиспользует topic; новый top-level id получает новый topic; nested id хранится как child parent-session и темы не получает.
6. State меняется custom emoji, title остаётся `[host] folder · ai-title` (короткий id только до появления title).
7. Permission traffic имеет приоритет над transcript traffic. Send limits и неизвестные mutation limits моделируются раздельно; все 429 используют `retry_after`.

### Parallelism и critical path

После bootstrap параллельны три lanes: transcript (`005→006→007`), Telegram (`008`) и transports (`010`). Spikes `003` и `004` можно выполнять параллельно с ними. После `011` hook `012` и agent `013` могут идти параллельно. `014` и `015` последовательно затрагивают общий routing/rendering path.

Critical path: `002 → 005 → 006 → 009 → 011 → 013 → 014 → 015 → 016`. `008` и `010` near-critical: задержка любого блокирует `011`.

## 4. Revised steps

Каждый блок ниже сохраняет `/maw-tasks` batch form.

---

### TASK-002: Bootstrap Cargo workspace and cctg CLI skeleton

Type: chore  
Mode: small-fix  
Priority: high  
Branch: chore/bootstrap-workspace  
Domains: transcript, hub, channel, hooks

**Scope.** Создать workspace из двух members: `crates/cctg` (единственный binary с `hub`, `agent`, `hook <event>`) и `crates/transcript` (pure library). Добавить только базовые shared dependencies (`tokio`, `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber`, `clap`), stderr logging и gitignore для `target/`, `.env`, `.cctg/`, `registry.json`. Никакой бизнес-логики.

- blocked by: nothing.

Acceptance criteria:
- [ ] `cargo build --workspace`, `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят на Windows
- [ ] workspace содержит ровно два package members и собирает ровно один executable `cctg`
- [ ] `cctg --help` показывает `hub`, `agent`, `hook`; обычный запуск не пишет ничего в stdout кроме вывода самой CLI-команды
- [ ] `transcript` не зависит от `tokio`, HTTP или filesystem crates
- [ ] build не создаёт незакоммиченные файлы вне игнорируемого `target/`

---

### TASK-003: Spike — nested claude session identity

Type: chore  
Mode: small-fix  
Priority: high  
Branch: chore/spike-nested-session-detection  
Domains: hooks

**Scope.** Временным локальным probe-hook записать только необходимые и отредактированные поля stdin, `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED` и process ancestry для interactive start и nested `claude -p`. Сравнить env id с stdin `session_id`; определить fallback через ppid. Probe и настройку удалить, production code не писать.

- blocked by: nothing.
- unblocks: TASK-012, TASK-015.

Acceptance criteria:
- [ ] `scratch/` содержит redacted captures для top-level и nested start без token, user id и private absolute paths
- [ ] findings явно отвечают, чей id видит nested hook и какие child/attended flags сохранены
- [ ] описан один `detect_parent(hook_input, env, process_tree) -> Option<SessionId>` contract и порядок fallback
- [ ] отдельно проверен случай отсутствующей/перезаписанной env variable
- [ ] временный hook удалён из settings, что подтверждено read-only проверкой

---

### TASK-004: Spike — development channel lifecycle

Type: chore  
Mode: small-fix  
Priority: high  
Branch: chore/spike-channel-lifecycle  
Domains: channel

**Scope.** На минимальном временном stdio JSON-RPC probe проверить fresh launch, `--resume`, `--continue` и запуск без `--dangerously-load-development-channels server:probe`; отдельно — user-scope server в новой папке. Зафиксировать banner, `/mcp`, inbound delivery, permission request и факт spawn server. Не использовать Node; probe и user config удалить.

- blocked by: nothing.
- unblocks: TASK-011, TASK-013.

Acceptance criteria:
- [ ] findings содержат таблицу 4 launch modes × banner/MCP/spawn/inbound/permission
- [ ] resume и continue результаты наблюдались, а не выведены из документации
- [ ] подтверждено или опровергнуто отсутствие per-project consent у user-scope server
- [ ] записана точная MVP launch command; wrapper явно out of scope либо обоснован отдельной задачей
- [ ] probe process/config удалены и не оставили секретов

---

### TASK-005: transcript — tolerant JSONL parser

Type: feature  
Mode: full  
Priority: high  
Branch: feature/transcript-parser  
Domains: transcript

**Scope.** Реализовать `parse(&str) -> Vec<Turn>`: allowlist `user`/`assistant`, только нужные поля/blocks с `#[serde(default)]`, unknown records/blocks пропускать. Отдельная pure-функция извлекает первый `ai-title`, не превращая его в turn. Добавить anonymized slices реальных JSONL.

- blocked by: TASK-002.

Acceptance criteria:
- [ ] unknown record/block и truncated final line не теряют ранее корректные turns и не panic
- [ ] empty/ignored-only input возвращает empty vector
- [ ] есть fixtures plain text, tool use/result и thinking+ai-title; ни одна не содержит private path, token или Telegram id
- [ ] `thinking` сохраняется только настолько, насколько нужно безопасно исключить его из render, но никогда не выдаётся наружу renderer-ом
- [ ] crate не выполняет IO и не использует `unwrap()` на входных данных

---

### TASK-006: transcript — brief/full rendering and Telegram sizing

Type: feature  
Mode: full  
Priority: high  
Branch: feature/transcript-renderers  
Domains: transcript

**Scope.** Реализовать brief/full по нормативным правилам. MVP output — plain text, чтобы не создавать невалидные Markdown entities. Split сначала по turns/lines, затем Unicode-safe hard split; вернуть chunks и явный признак «предпочесть file» выше заданного порога. Размер проверять по тому же определению, которое использует adapter, с integration fallback на file при Telegram 400.

- blocked by: TASK-005.

Acceptance criteria:
- [ ] каждый text chunk не превышает 4096 characters и не разрезает UTF-8 sequence
- [ ] brief содержит ровно по одной строке на tool call без inputs/results; full добавляет inputs и truncated results
- [ ] ни один renderer не выдаёт thinking
- [ ] 50 KB single block и Unicode/emoji boundary дают deterministic chunks или file recommendation
- [ ] 5000-turn synthetic render не имеет квадратичного роста и укладывается в зафиксированный разумный benchmark

---

### TASK-007: transcript — subagent data and collapsed rendering

Type: feature  
Mode: full  
Priority: medium  
Branch: feature/transcript-subagents  
Domains: transcript

**Scope.** Добавить pure parsers для содержимого subagent JSONL и optional `.meta.json`, плюс collapsed brief model. Библиотека принимает строки и fallback metadata, не открывает path. Handback message принимается отдельным optional input и имеет приоритет над transcript final text; выбор файла/чтение остаются в hub.

- blocked by: TASK-005.
- prefer after: TASK-006.

Acceptance criteria:
- [ ] sidechain fixture рендерится одним `↳ <type> <id>` block и не попадает в top-level turns
- [ ] meta description используется при наличии; malformed/missing meta даёт безопасный fallback
- [ ] supplied handback message становится body вместо closing text
- [ ] subagent body всегда brief даже из full parent view
- [ ] API принимает `&str`/typed values и не содержит filesystem IO

---

### TASK-008: hub — teloxide foundation and outbound scheduler

Type: feature  
Mode: full  
Priority: high  
Branch: feature/hub-telegram-foundation  
Domains: hub

**Scope.** Подключить `teloxide 0.17.0` с минимальными подходящими features, `.env` config, long polling и allowlist gate по `from.id`. Все sends/edits/topic mutations проходят через один scheduler с per-topic ordering и permission priority. Документированные send limits моделировать отдельно; edits/topic mutations сериализовать/coalesce без выдуманного numeric limit; любой 429 ждёт `retry_after`. Зафиксировать release binary size, clean-build time и idle RSS как evidence для будущего fallback decision.

- blocked by: TASK-002.

Acceptance criteria:
- [ ] non-allowlisted sender не достигает handlers; logs не содержат sender id
- [ ] send scheduler соблюдает 20 group messages/minute в mocked-time test и сохраняет порядок внутри topic
- [ ] repeated edits coalesce; mocked 429 повторяется не раньше `retry_after` и не создаёт retry storm
- [ ] topic creation проходит через mutation lane, но не списывается из неподтверждённого «message bucket»
- [ ] token/secret не появляются в captured logs ни на одном error path
- [ ] task notes содержат измеренные binary/build/RSS и подтверждение наличия create/edit forum methods; смена library без отдельного решения не производится

---

### TASK-009: hub — local transcript commands

Type: feature  
Mode: full  
Priority: high  
Branch: feature/hub-transcript-commands  
Domains: hub, transcript

**Scope.** Первый vertical slice: `/brief [n]` и `/full [n]` читают переданный local transcript path, используют `transcript`, отвечают chunks через scheduler или document при превышении threshold. Update offset сохраняется атомарно, чтобы restart не дублировал команды. Registry ещё не требуется.

- blocked by: TASK-006, TASK-008.

Acceptance criteria:
- [ ] `/brief` и `/full` на fixtures совпадают с library output и сохраняют порядок
- [ ] большой output отправляется document, а Telegram 400 из-за размера переключает delivery на document один раз
- [ ] persisted offset исключает повторную обработку update после simulated restart
- [ ] unreadable/missing path даёт user-readable error и polling продолжает работать
- [ ] logs/tests/fixtures не содержат token, реальные user ids или private paths

---

### TASK-010: cctg transport contracts — agent TCP and hook HTTP ingress

Type: feature  
Mode: full  
Priority: high  
Branch: feature/transport-contracts  
Domains: hub, channel, hooks

**Scope.** Во внутренних модулях `cctg` определить versioned newline-JSON protocol для persistent agent↔hub TCP (shared-secret first message, line cap, reconnect только у agent) и отдельные serde payloads/HTTP endpoint для one-shot hook POST. Listener по умолчанию loopback; non-loopback требует явной config. Никакого `proto` crate и никакого reconnect loop у hook.

- blocked by: TASK-002.

Acceptance criteria:
- [ ] все TCP variants round-trip; unknown version/kind даёт контролируемую ошибку без panic
- [ ] неверный secret отвергается до Register и не попадает в logs; oversized line закрывает connection с bounded allocation
- [ ] agent reconnect/backoff и re-Register доказаны restart test
- [ ] hook endpoint принимает один authenticated POST и возвращает быстро; duplicate event имеет idempotency key/поведение
- [ ] loopback default и explicit non-loopback config покрыты тестами

---

### TASK-011: hub — registry and topic lifecycle

Type: feature  
Mode: full  
Priority: high  
Branch: feature/hub-registry-topics  
Domains: hub

**Scope.** Реализовать нормативный registry и атомарное persistence/reconciliation. Top-level new session id создаёт topic; resume того же id переиспользует; nested получает `parent_session_id` и темы не получает. Hook может создать session/topic до agent connection. На SessionEnd topic остаётся открытым, становится dead custom-emoji; waiting permission также custom-emoji. Title строго `[host] folder · ai-title`, short id только пока title отсутствует.

- blocked by: TASK-008, TASK-010.
- prefer after: TASK-004.

Acceptance criteria:
- [ ] same session id после restart/resume создаёт zero new topics; новый top-level id создаёт ровно одну новую тему
- [ ] nested Register/Hook event создаёт zero topics и ссылается на parent topic
- [ ] SessionEnd не закрывает topic; dead/waiting/alive меняют допустимый `icon_custom_emoji_id`
- [ ] title всегда ≤128 characters и сохраняет host/folder; short id заменяется ai-title при его появлении
- [ ] interrupted persistence оставляет предыдущий valid registry; `TOPIC_ID_INVALID` очищает только точную stale mapping и создаёт одну замену
- [ ] hook-only session видима как «channel not connected», а поздний agent связывается без duplicate topic

---

### TASK-012: hook — lifecycle events and handback capture

Type: feature  
Mode: full  
Priority: high  
Branch: feature/hook-subcommand  
Domains: hooks

**Scope.** `cctg hook <event>` читает stdin, десериализует только нужные fields и делает один authenticated HTTP POST с коротким timeout, всегда exit 0. Поддержать шесть lifecycle events (`SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`) и узкий `PostToolUse` matcher для `SubagentHandback`, передающий `tool_input.message`. Nesting — по результату `TASK-003`.

- blocked by: TASK-010, TASK-011.
- prefer after: TASK-003.

Acceptance criteria:
- [ ] каждый из шести lifecycle payloads даёт ожидаемый HTTP event; Handback matcher передаёт только нужный message
- [ ] unreachable hub: exit 0 в пределах configured short timeout, stdout empty, stderr без входных payload/secrets
- [ ] nesting/parent id соответствует зафиксированному правилу для top-level и nested cases
- [ ] SessionEnd укладывается существенно ниже default 1.5 s; malformed/empty stdin также exit 0 без panic
- [ ] user-scope settings snippet регистрирует все события и не содержит machine-specific secret/path

---

### TASK-013: agent — Channel MCP server over stdio

Type: feature  
Mode: full  
Priority: high  
Branch: feature/agent-channel-server  
Domains: channel

**Scope.** Реализовать нормативную hand-rolled JSON-RPC surface, persistent TCP connection к hub и matching session id через env/hook registry. Meta keys только `[A-Za-z0-9_]+`; invalid keys отбрасываются, не нормализуются молча в другое имя. User-scope install command использует absolute executable path. Nested child agent, если был spawned, не регистрируется как самостоятельный routable channel.

- blocked by: TASK-010.
- prefer after: TASK-004, TASK-011.

Acceptance criteria:
- [ ] scripted initialize/initialized/tools-list/tools-call flow выдаёт по одной valid JSON object на stdout line
- [ ] unknown method возвращает `-32601`; malformed external input не panic и не убивает server без контролируемого ответа
- [ ] stdout никогда не содержит logs/panic prose, включая hub outage; logging только stderr/file
- [ ] invalid meta key отбрасывается, valid keys сохраняются byte-for-byte
- [ ] hub reconnect не завершает MCP server и повторно регистрирует ту же top-level session
- [ ] manual channel run подтверждает banner и inbound delivery; nested launch не создаёт отдельную routable registration

---

### TASK-014: permission relay end to end

Type: feature  
Mode: full  
Priority: high  
Branch: feature/permission-relay  
Domains: channel, hub

**Scope.** Relay permission request в parent topic с Allow/Deny buttons, приоритетом scheduler и allowlist gate. Callback data содержит только action и 5-letter request id и укладывается в 64 bytes. Первый ответ побеждает; поздний verdict становится harmless resolved state. Поскольку Claude уже ограничивает/sanitizes preview, hub всё равно ограничивает итоговый Telegram message.

- blocked by: TASK-011, TASK-013.

Acceptance criteria:
- [ ] Allow/Deny callbacks ≤64 bytes и порождают ровно один matching verdict
- [ ] callback от неразрешённого sender не отправляет verdict и не раскрывает request details
- [ ] второй/late callback idempotent и помечает prompt resolved без второго verdict
- [ ] длинный preview даёт message ≤4096 characters и permission traffic обгоняет queued transcript traffic
- [ ] manual live check подтверждает approval из Telegram и закрытие параллельного terminal prompt

---

### TASK-015: subagents and nested runs in parent topic

Type: feature  
Mode: full  
Priority: high  
Branch: feature/subagents-nested-routing  
Domains: hub, hooks, transcript

**Scope.** Связать explicit parent `Agent` call/result с `agent_id`, отображать только коррелированные subagents, а empty/internal unmatched events не превращать в ghost blocks. На stop body выбирается: captured Handback message → brief `agent_transcript_path` → `last_assistant_message`. Nested hook registration прикрепляется к parent topic как `⇣ nested <id>` и не создаёт topic/channel route. Reply к subagent block идёт parent session с `target_agent`.

- blocked by: TASK-007, TASK-011, TASK-012, TASK-013.
- prefer after: TASK-014.

Acceptance criteria:
- [ ] три explicit subagents дают одну parent topic и ровно три correlated blocks
- [ ] empty и non-correlated internal `SubagentStop`, включая fixture с main `--agent`, не создают ghost block
- [ ] Handback fixture показывает delivered report, а fallback order покрыт тестами
- [ ] nested `claude -p` даёт zero new topics и один parent nested block
- [ ] reply к block приходит только в parent channel с valid `target_agent` meta
- [ ] restart hub не создаёт duplicate topic/block; stale in-progress block помечается детерминированно

---

### TASK-016: multi-session routing soak

Type: chore  
Mode: full  
Priority: medium  
Branch: chore/multi-session-soak  
Domains: hub, channel, hooks

**Scope.** QA gate: четыре одновременных запуска (два top-level в одной папке, один top-level в другой, один nested) плюс пятый top-level, начатый при выключенном hub. Ожидаются **четыре** topics: три от первой группы и один от пятого; nested исключён. Проверить routing, recovery, priorities, message counts и registry.

- blocked by: TASK-014, TASK-015.

Acceptance criteria:
- [ ] пять launches дают ровно четыре topics и ни одного cross-route
- [ ] session, начатая при offline hub, после возврата получает одну тему без duplicate
- [ ] burst соблюдает scheduler policy; каждый 429 обработан по `retry_after`, без retry storm
- [ ] permission prompt проходит впереди transcript queue с измеренной latency
- [ ] итоговый registry точно соответствует четырём topics и parent relation nested run
- [ ] report фиксирует send/edit/topic-create counts отдельно и не сравнивает edits с неподтверждённым 20/min limit

## 5. Risk areas

- **Channel API остаётся research preview.** Custom server требует development flag при каждом launch; missing flag не даёт delivery acknowledgement. `TASK-004` обязан зафиксировать реальные resume semantics, а hub — явно показывать отсутствие channel connection.
- **Nested detection опирается на недокументированную env.** `CLAUDE_CODE_SESSION_ID` отсутствует в official hooks environment contract; нужен один изолированный detection function и проверенный ppid fallback.
- **Subagent/internal-agent ambiguity.** Empty `agent_type` — не единственный internal case. Корреляция с explicit parent Agent call обязательна; непарные events лучше не показать, чем создать ложный Telegram block.
- **Transcript lag.** На `Stop` текущий final text берётся из hook `last_assistant_message`; JSONL используется для history и может отставать.
- **Telegram flood control.** 20 messages/minute относится к group sends, но numeric edit/topic mutation rate не опубликован. Приоритеты, coalescing и `retry_after` важнее фиктивного универсального token bucket.
- **Dead-session buffer не имеет нормативной retention policy.** MVP должен хранить очередь так, чтобы restart не терял её; предельный размер/eviction требует явного решения до реализации unbounded persistence. Это concern, а не выдуманный default.
- **Windows filesystem/process details.** Atomic replacement `registry.json`, Unicode/path encoding и ppid traversal должны тестироваться на Windows, а не переноситься из Linux reference без проверки.
- **Secret and identity leakage.** Bot token может попасть в URL/error chain; shared secret и Telegram ids нельзя логировать или фиксировать literals в tests/fixtures. Tests должны генерировать ephemeral values runtime и проверять redaction.
- **Dependency freshness.** `teloxide 0.17.0` отстаёт по новым Bot API fields, но нужная MVP surface присутствует. Переход на `frankenstein`/raw reqwest допускается только по конкретному отсутствующему API или измеренному operational cost, не по возрасту release сам по себе.

# PLAN_V2 — TASK-013: agent, Channel MCP server over stdio

`T` = `maw/tasks/in_progress/TASK-013`. Референс — `T/scratch/planner/ws`, патч — `T/scratch/planner/task013.patch`. Патч полезен как baseline, но не является готовым финальным результатом: после применения обязательны исправления ниже, поэтому `hashes.txt` годится только для проверки baseline до исправлений.

## 1. Review notes

### Проверенная disconfirmation и сборка

- Контрпример до общей оценки: MCP `ping` должен получить пустой `result`, а референс отвечает `-32601`. Он подтвердился в `scratch/planner/ws/crates/cctg/src/channel.rs:258-300` и тестах `channel.rs:519-544`, `tests/agent_stdio.rs:123-132`. `OPEN_DECISIONS.md` также требует `{}`. Доказательство: `T/scratch/reviewer1_disconfirmation.md`.
- Референс независимо скопирован в `T/scratch/reviewer1/ws` и собран с target под `%TEMP%`. Первый debug-build упёрся в MSVC `LNK1104`/`D8050`; последовательный повтор с `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1` прошёл: 305 passed, 0 failed, 1 ignored. `clippy -D warnings`, `cargo fmt --check`, `git diff --check` чисты. Логи — в `T/scratch/reviewer1/`.
- Прямая stdio-проба подтвердила JSONL-only stdout, контролируемые ошибки и отсутствие синтетического секрета/payload в install output. Она также воспроизвела ошибки ниже (`protocol_stdout.jsonl`, `protocol_stderr.txt`, `install_stdout.txt`).
- `clear_evidence.txt`/`probe_log_clear.jsonl` подтверждают: `/clear` не рестартует MCP, pid и старый env session id сохраняются. `live_hub.jsonl` подтверждает register/inbound/reply/permission и отсутствие второго hub-link от вложенного `sdk-cli`.

### Ошибки исходного плана

1. **`ping` неверен.** Сейчас он попадает в общий `-32601`, а тест закрепляет ошибку. MCP требует `{ "result": {} }`: https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/ping.
2. **Не проверяется `jsonrpc: "2.0"`.** `on_line` классифицирует объект только по `method`/`id`; запрос без `jsonrpc` реально принят. Missing/wrong `jsonrpc` должен дать `-32600`, `id: null`; scalar `params` тоже не должен трактоваться как отсутствие параметров: https://www.jsonrpc.org/specification.
3. **Version negotiation неверен.** Референс эхоирует любую строку, включая проверенную `9999-99-99`. MCP разрешает эхо только поддерживаемой версии; иначе сервер возвращает поддерживаемую: https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle. Здесь поддерживается `2025-11-25`, реально присылаемая Claude Code 2.1.280. Missing/non-string version в `initialize` — `-32602`, не fallback.
4. **Duplicate permission id остаётся открытым после первого verdict.** `VecDeque` принимает дубликаты; два одинаковых requests приводят к двум outgoing verdicts. Падающий атакующий тест: `T/scratch/reviewer1/ws/crates/cctg/tests/reviewer_probe.rs`. Уже открытый id нужно не пересылать и не добавлять повторно.
5. **Финальная сверка исходных hashes невозможна.** После исправлений минимум `channel.rs` и `agent_stdio.rs` отличаются. `hashes.txt` проверяет только baseline сразу после patch.
6. **Нет negative live-run без development flag.** TASK-004 доказал: agent спавнится как обычный MCP, но channel notifications тихо отбрасываются. Agent не может отличить режим и подключается к hub как interactive `cli`. По `OPEN_DECISIONS.md` confirmed flag не добавляется, но план обязан проверить этот режим и не обещать достоверный no-channel state.
7. **Open questions уже закрыты.** Routing вынесен в TASK-021 (после TASK-013, блокирует TASK-014/016); confirmed flag сейчас не нужен; `ping` получает `{}`. Раздел open questions удаляется.

### Подтверждённые части референса

- Hand-rolled `serde_json` соответствует закону; dependency change только `tokio/io-std`, без новых crates/lockfile.
- Один stdout writer, tracing явно в stderr, отдельные stdin/hub readers сохраняют framing и purity.
- Unknown/malformed tool call — protocol error; плохой `reply.text`, no hub, full queue — tool result `isError: true`, как требует MCP Tools: https://modelcontextprotocol.io/specification/2025-11-25/server/tools.
- Meta filter `[A-Za-z0-9_]+` и неизменные string values соответствуют https://code.claude.com/docs/en/channels-reference.
- Caps 128 KiB reply и 32 KiB permission field держат escaped wire JSON ниже `MAX_LINE = 1 MiB`.
- Optional/default `Register.claude_pid` обратно совместим в wire v1.

## 2. Updated understanding

На HEAD `Command::Agent` пуст, `agent.rs` содержит только TCP link TASK-010. `wire.rs` уже имеет register/reply/permission/inbound/verdict; `device.rs` — общие с hook host/canonical cwd. Hub владеет registry/slots, но topic↔agent routing ещё отсутствует и принадлежит TASK-021.

Референс добавляет: (1) чистый `channel.rs`; (2) stdio loop поверх reconnecting link; (3) optional pid и hub-side `/clear` rebinding; (4) рабочие `agent` и печатающий, но не выполняющий `agent-install`.

Платформенные границы:

- Interactive + development flag: полный channel.
- Interactive без flag: agent спавнится, inbound тихо теряется, надёжного channel-enabled signal нет.
- Любой `claude -p` (nested и hub-started resume) — `sdk-cli`; в TASK-013 MCP отвечает, но hub-link не создаёт. Это временно приемлемо, TASK-019 использует one-shot prompt/stream-json.
- `/clear` сохраняет MCP process, поэтому binding переносит hub по `(host, claude_pid)` после нового `SessionStart(source=clear)`.
- Без увиденного SessionStart slot не создаётся; nested session никогда не получает topic channel.

## 3. Revised approach

### A. Строгая протокольная машина

Оставить `channel::Server` без IO, но до dispatch валидировать top-level object, точное `jsonrpc: "2.0"`, строковый `method`, request id string/number и structured `params`. Parse error — `-32700`; валидный JSON с плохим envelope — `-32600`, `id: null`. Неизвестный request-method — `-32601`; неизвестная notification игнорируется.

Поддержать `initialize`, `ping`, `tools/list`, `tools/call`; `ping` возвращает `{}`. `initialize` требует непустой string `protocolVersion`, объявляет tools/channel/permission и отвечает поддерживаемой `2025-11-25`. Server notifications выходят только после `notifications/initialized`; до неё bounded FIFO 64.

Permission relay принимает пятибуквенные lowercase id без `l`, пересылает каждый одновременно открытый id один раз, хранит максимум 64 и выпускает verdict ровно один раз только для открытого id.

### B. Stdio и hub-link

Сохранить дизайн референса: blocking reader thread читает строки с limit 8 MiB и дочитывает хвост oversized frame; async loop один владеет stdout, пишет JSON object + `\n` и flush. Закрытие stdin завершает MCP/link. Payload, secret и serde error с входным значением не логируются.

Interactive `cli` с session id/config регистрирует shared host/canonical cwd и pid собственного Claude через `proctree`, не env `CLAUDE_PID`. При absent/crashed hub MCP живёт, link reconnects/backoff и повторяет register. `sdk-cli`, no session и no config работают как `Hub::Off` без TCP.

### C. `/clear` на hub-side

Добавить optional `Register.claude_pid` без смены wire version. Registry принимает agent только для live top-level. Slots предпочитает live register session, затем live top-level того же `(host,pid)`. После `SessionStart` переносит все connections host/pid на новую live session, очищая старую registry/pending связь. Nested session не привязывается.

### D. Installation и scope

`cctg agent-install` только печатает `claude mcp add --scope user cctg -- "<absolute exe>" agent`; не исполняет команду и не печатает config/secrets. Никаких записей в `~/.claude.json`.

### E. Границы

Не добавлять topic↔agent routing (TASK-021), Telegram permission UI (TASK-014), transcript streaming (TASK-016), headless resume transport (TASK-019), channel-confirmed heuristic, scheduler/Bot API changes или wire v2.
## 4. Revised steps

### Шаг 0. Предусловия

- Проверить `git status`; не затирать пользовательские изменения и не менять ветку без orchestrator.
- Cargo запускать по одному с target под `%TEMP%`; при MSVC resource error — `-j 1`, `CARGO_PROFILE_DEV_DEBUG=0`.
- Не читать `.env`, не обращаться к Telegram API, не регистрировать MCP server в user config.

### Шаг 1. Взять reference как baseline, не финал

1. `git apply --check T/scratch/planner/task013.patch`, затем применить patch.
2. Сразу после этого можно выполнить `verify_hashes.sh` для доказательства точного baseline из 13 файлов.
3. После следующих шагов не требовать совпадения исправленных файлов с `hashes.txt`; source of truth — diff и tests.

### Шаг 2. Исправить MCP в `crates/cctg/src/channel.rs`

1. Проверить `jsonrpc == "2.0"` до dispatch; wrong/missing value — `-32600`, без эха input.
2. Отклонять scalar `params`, bad id и прочий плохой envelope контролируемым protocol error.
3. Заменить arbitrary version echo/fallback: `initialize` требует непустой string version; для `2025-11-25` возвращает её, для неизвестной — поддерживаемую `2025-11-25`.
4. Добавить `"ping" => result(id, json!({}))`; прочие неизвестные requests оставить `-32601`.
5. В `on_permission_request` до `try_send` отбросить id, уже присутствующий в `open_permissions`; хранить одну запись. Verdict удаляет её до emission, поэтому repeated/stray verdict ничего не выдаёт.
6. Сохранить capabilities, reply schema, meta filter, pre-init queue и size caps.

### Шаг 3. Реализовать stdio agent/config

- `Cargo.toml`: добавить только `tokio/io-std`.
- `device.rs`: `CCTG_HUB_AGENT_ADDR`/`agent_addr`, default `127.0.0.1:47291`; сохранить env → `~/.cctg/device.env`, без `set_var`.
- `agent.rs`: `run_stdio`, `link_plan`, framed reader, oversized drain, single-writer `serve_channel`, `install_command`; reconnect core не переделывать.
- `lib.rs`: экспортировать `channel`.

### Шаг 4. Wire и `/clear`

- `wire.rs`: optional/default `Register.claude_pid`; обновить register literals и backward-compat test; `VERSION = 1`.
- `registry.rs`: связывать agent только с `TopLevel`; добавить lookup live top-level по host/pid и проверку live session.
- `slots.rs`: connection хранит session/host/pid/sender; pid fallback только если env session уже не live top-level; после `SessionStart` выполнять `follow_pid`.
- Проверить End→Start, Start→late End и reconnect после clear со старым env id.
- `Reply`/`Inbound` с Telegram здесь не маршрутизировать — TASK-021.

### Шаг 5. CLI

- `main.rs`: `Command::Agent` ставит fixed-text panic hook на stderr, ждёт `run_stdio`, затем exit 0, чтобы blocking stdin thread не держал runtime.
- Tracing агента — plain stderr без ANSI/time; stdout принадлежит только JSON-RPC writer.
- `AgentInstall` берёт `current_exe`, canonicalizes Windows path и печатает registration command.

### Шаг 6. Автоматические тесты

В `channel.rs`:

- `ping` → `{}`, соседний unknown method → `-32601`.
- Missing/wrong jsonrpc, scalar params, bad ids, batch, non-UTF8, truncated/oversized input дают controlled answers; следующий valid request работает.
- Unknown version не эхоируется; missing/non-string version → `-32602`; `2025-11-25` согласуется.
- Duplicate permission request даёт один `AgentMsg`; первый verdict — одну notification, repeated/stray — ничего.
- Pre-init FIFO сохраняет порядок и bound; meta values/filter и size caps остаются покрыты.

В `tests/agent_stdio.rs`:

- Real binary script: initialize → initialized → ping → tools/list → valid tools/call → bad reply input → unknown method → malformed input → valid request.
- Каждая stdout line — один JSON object с `jsonrpc: "2.0"`; число lines соответствует только requests/errors.
- Failing hub реально вызывает tracing; stdout не содержит logs/hub text/synthetic secret/payload, stderr не содержит secret/payload.
- Headless `sdk-cli`, no session, no config отвечают MCP, `reply.isError == true`, TCP отсутствует.
- Process-level `agent-install` с synthetic secret: stdout содержит absolute path/`--scope user`, но не secret/config.

В agent/hub tests:

- Reconnect/re-register той же session, queued reply during downtime, inbound/meta и permission round trip.
- Nested session never binds.
- Clear rebinding/reconnect после clear.
- Exact MAX_RPC_LINE boundary и валидная строка после oversized input.

### Шаг 7. Полная проверка

```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task013-target'
cargo test --workspace --offline -j 1
cargo clippy -p cctg --all-targets --offline -j 1 -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Не фиксировать 305 как вечный oracle: после regression tests число вырастет. Требование — 0 failed и сохранённый существующий ignored test.

### Шаг 8. Изолированные live checks

Только temporary `--mcp-config`, synthetic secret, binary copy под `%TEMP%`, `fake_hub.py`; user config не менять.

1. **С development flag:** interactive launch; banner либо debug `Channel notifications registered`; inbound доходит, `reply` приходит mock hub, permission request/verdict корректны.
2. **Без flag:** agent спавнится как обычный MCP, не падает, mock hub видит register, injected notification Claude Code молча не доставляет. Зафиксировать: TASK-013 не умеет достоверно поставить no-channel icon и не добавляет heuristic.
3. **Nested `claude -p`:** `sdk-cli` отвечает MCP, но mock hub не получает второго connection/register.
4. **Headless top-level/resume:** тоже `sdk-cli`, поэтому no hub-link; это ожидаемо сейчас и передано TASK-019 one-shot transport.
5. **`/clear`:** один MCP pid и старый env id; новый hook session вызывает hub rebinding, агент не рестартует.
6. Удалить только temp artifacts. Уже оставшиеся Claude-created `projects[...cctg13_clear_probe/live]` keys не чистить автоматически; это user-side cleanup по решению orchestrator.
### Матрица приёмки

| Критерий | Конкретная проверка |
|---|---|
| initialize → initialized → tools/list → tools/call; JSONL | channel unit + expanded `agent_stdio` |
| unknown `-32601`, bad input controlled | envelope/truncation/non-UTF8/oversize tests и valid request после ошибок |
| stdout purity при absent hub | failing-hub process test с real tracing event и panic hook |
| meta filter/value preservation | existing meta unit + end-to-end relay |
| reconnect/re-register | hub restart + queued reply tests |
| manual banner/inbound; nested no register | flag live-run + nested `sdk-cli` run |
| shared cwd/host helpers | register construction + mock hub assertion |
| `/clear` | supplied live evidence + both event orders + reconnect tests |
| MCP utility/version | ping empty-result + negotiation tests |
| permission only while open | duplicate request и stray/repeated verdict tests |
| install absolute path/no secret | unit quote + process test |
| existing tests | workspace + clippy + fmt + diff-check |

## 5. Risk areas

- **No development flag не детектируется надёжно.** Initialize/tools выглядят нормально, notification не имеет ack. Confirmed flag/heuristic по решению orchestrator не добавляется.
- **`sdk-cli` означает headless, не nested.** Правило защищает от nested `-p`, но отключает hub-started headless resume; TASK-019 использует one-shot prompt/stream-json.
- **PID может отсутствовать** у npm-only/node parent или оборванной process chain; тогда `/clear` не rebind-ится до рестарта MCP. PID reuse смягчается host key и hooks, но lost hooks остаются риском.
- **In-flight TCP write может потеряться.** Outbox гарантирует очередь до write, но не ack/replay; это контракт TASK-010.
- **Pre-init queue = 64.** Старейшие notifications выпадают; долговременный buffer принадлежит hub.
- **Topic↔agent routing отсутствует.** TASK-021 должна связать payload до TASK-014/016; TASK-013 доказывает transport на mock hub, не Telegram E2E.
- **Banner зависит от UI.** На 2.1.280 notice сворачивается; fallback — debug registration line и фактический inbound.
- **Windows build чувствителен к ресурсам.** Параллельный debug build воспроизвёл `LNK1104/D8050`, последовательный nodebug прошёл.
- **Reference hashes устаревают после fixes.** Не копировать `planner/ws` поверх исправленного кода и не считать `13 x OK` финальным gate.
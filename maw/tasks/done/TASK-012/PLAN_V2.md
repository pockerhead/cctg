# PLAN V2 — TASK-012: hook, lifecycle events и отчёт субагента

## 1. Review notes

### Что проверено

- Обязательный disconfirmation-кейс: вложенный запуск при npm-установке, где собственный и родительский Claude видны как `node.exe`. Контрпример подтвердился. В `scratch/planner/ws/crates/cctg/src/proctree.rs` функция `lineage()` сначала выбирает ближайший процесс с именем `claude`, а `CLAUDE_PID` использует лишь как fallback; родителя она ищет только по имени `claude`. Изолированный probe дал:
  - native: `claude_pid=30`, `parent_claude_pid=10` — верно;
  - npm-only: `claude_pid=30`, `parent_claude_pid=None` — вложенность потеряна;
  - mixed (`node.exe` собственной сессии над native `claude.exe`-родителем): `claude_pid=10`, `parent_claude_pid=None` — pid родителя ошибочно принят за собственный.
  Результат и probe сохранены в `scratch/reviewer1_disconfirmation.md` и `scratch/reviewer1_probe/`. Встроенный тест с комментарием «npm install» закрепляет mixed-ошибку как ожидаемое поведение, поэтому зелёные тесты её не обнаруживают.
- Это не требует полной поддержки npm-установки: нормативный контекст говорит «No Node on any machine». Но `CLAUDE_PID`, найденный ниже первого именованного `claude`, нельзя игнорировать — иначе даже собственный pid для `SessionEnd` становится pid родителя. Npm-only parent detection остаётся явно неподдерживаемым риском, а не якобы покрытым тестом.
- В `hook.rs` фильтр typed internal `SubagentStop` применяется только при `Some(agent_transcript_path)`. Событие с непустым `agent_type`, но отсутствующим/пустым `agent_transcript_path`, проходит в hub. Это не полностью выполняет критерий «`--agent`-шум без соответствующего SubagentStart отбрасывается». Официальная документация подтверждает, что internal agents могут нести имя `--agent`; для настоящего `SubagentStop` документирован собственный `agent_transcript_path`. Поэтому для typed stop надо требовать непустой путь и существование `.jsonl` или `.meta.json`, не опираясь на ранее замеченный `SubagentStart`.
- Единый POST timeout 500 мс корректен по бюджету `SessionEnd`, но создаёт измеримую задержку на каждом блокирующем событии. Собственный black-hole probe дал 542 мс, exit 0, пустой stdout и безопасный stderr. Ответивший локальный hub в материалах planner занимает 18–39 мс. Для `UserPromptSubmit`, `Stop`, subagent events и handback безопаснее использовать 300 мс: это оставляет более чем семикратный запас относительно наблюдавшегося ответа и уменьшает задержку остановленного hub на 40%. Для `SessionStart`/`SessionEnd` оставить 500 мс ради надёжности lifecycle-событий.
- Интеграционные тесты референса проверяют silent hub и закрытый порт, но не зависший открытый stdin. Отдельный probe подтвердил, что текущая оболочка завершается за 329 мс с exit 0 и пустым stdout; этот сценарий нужно закрепить автоматическим тестом.
- План нельзя исполнять как «применить патч и сверить старые sha256». `PLAN.md` называет HEAD `8cc81ea`, фактический HEAD при ревью — `7f87c3f6…`. `git apply --check` сейчас проходит, а все 18 опубликованных хэшей совпадают с байтами reference workspace, но после обязательных исправлений итоговые файлы закономерно перестанут совпадать со старыми `hashes.txt`.
- Остальная архитектура reference подтверждена кодом:
  - `hook::post` — один TCP/HTTP POST под единым timeout без retry, с безопасными `PostError`;
  - wire contract уже содержит все семь вариантов (`6` lifecycle/subagent событий + `SubagentHandback`) и лимит 1 MiB;
  - hub трактует любой `parent_claude_pid: Some` как nested/unknown-parent и проверяет чужой `SessionEnd.claude_pid`;
  - config читается из process env и `~/.cctg/device.env` без `set_var`, process env имеет приоритет;
  - `windows-sys 0.61.2` уже есть в lockfile, а выбранные features минимальны для ToolHelp + `CloseHandle`; валидный snapshot закрывается на каждом обычном пути;
  - settings snippet содержит все события, не содержит секретов или машинных путей и использует принятый orchestrator-ом shell form с одним `PostToolUse` matcher.
- Референс пересобран самостоятельно с `CARGO_TARGET_DIR` под `%TEMP%`: `cargo test --workspace --offline -j 2` — 183 passed, 1 ignored; интеграционные и doc tests зелёные; `cargo clippy -p cctg --all-targets --offline -- -D warnings` и `cargo fmt --all -- --check` прошли.

### Проверка по актуальным первичным источникам

- [Claude Code hooks reference](https://code.claude.com/docs/en/hooks) подтверждает: command hooks получают JSON через stdin; plain stdout при exit 0 попадает в контекст для `SessionStart` и `UserPromptSubmit`; `UserPromptSubmit` блокирует обработку промпта; `SessionEnd` имеет общий default-бюджет 1.5 с; shell form на Windows использует Git Bash либо PowerShell; `SubagentStop` несёт `agent_transcript_path` и `last_assistant_message`; internal agents могут иметь пустой `agent_type` либо имя session-level `--agent`; отчёт `SubagentHandback` находится в `tool_input.message` у `PreToolUse`/`PostToolUse`.
- [Tokio stdin documentation](https://docs.rs/tokio/latest/tokio/io/fn.stdin.html) подтверждает, что async stdin использует неотменяемое blocking-чтение и может задержать shutdown runtime; отдельный OS thread плюс принудительный `process::exit(0)` здесь оправданы.
- [Microsoft CreateToolhelp32Snapshot documentation](https://learn.microsoft.com/en-us/windows/win32/api/tlhelp32/nf-tlhelp32-createtoolhelp32snapshot) требует закрывать snapshot через `CloseHandle`; reference это делает и не удерживает handle после построения таблицы.

## 2. Updated understanding

- В текущем репозитории `Command::Hook { event }` — no-op; `crates/cctg/src/hook.rs` содержит только проверенный TASK-010 transport. Wire, strict hook ingress, dedup и TASK-011 registry уже готовы и менять их контракт не нужно.
- Устройство должно сформировать `HookPost { v, event_id, host, session_id, cwd, transcript_path, event }`. Десериализуются только используемые поля через узкие `#[serde(default)]` структуры; текст prompt никогда не уходит в hub.
- `SessionStart` и `SessionEnd` единственные события, которым нужен snapshot дерева процессов. `SessionStart` отправляет собственный pid и pid следующего распознанного Claude-предка; `SessionEnd` — только собственный pid.
- Hub, а не hook, переводит `parent_claude_pid` в три состояния TASK-003: top-level (`None`), nested с известным parent и nested с неизвестным parent. Любой реальный ancestor pid должен быть `Some`; отсутствие регистрации ancestor не должно превращать запуск в top-level.
- На поддерживаемой установке Claude процесс называется `claude.exe`/`claude`. `CLAUDE_PID` — дополнительное подтверждение собственного процесса и защита для mixed name case. Полностью npm-only дерево нельзя надёжно разобрать по одному ToolHelp image name без чтения command line; по проектному закону это вне deployment scope.
- Реальные TASK-003 captures дают 14 internal `SubagentStop` с пустым `agent_type`: путь в payload есть, но ни `.jsonl`, ни `.meta.json` по нему нет. У настоящего Explore есть `SubagentStart`, непустой type и оба файла. Нельзя полагаться на in-process память о `SubagentStart`, потому что hooks могли установиться посреди сессии; файловый сигнал является stateless fallback.
- `SubagentHandback` не универсален. Согласно решению orchestrator-а snippet регистрирует только `PostToolUse`; бинарник всё равно принимает `PreToolUse`, чтобы корректно обработать вручную установленный matcher или будущую смену настройки.
- Конфигурация hook/agent: process env имеет приоритет над `~/.cctg/device.env`; обязательный `CCTG_HUB_SECRET`, optional `CCTG_HUB_HOOK_ADDR` и `CCTG_HOST`; значения не переносятся в process env и не попадают детям.
- `cwd` канонизируется на устройстве через `std::fs::canonicalize`; при ошибке остаётся исходным; успешный Windows path очищается от обычных `\\?\`/`\\?\UNC\` префиксов. Hub затем делает только lexical normalization.

## 3. Revised approach

Reference patch использовать как стартовую реализацию, но не как готовый артефакт. Сохранить его общую декомпозицию (`device`, `proctree`, pure `hook::build`, тонкий `hook::run`) и внести три целевых изменения:

1. Исправить выбор собственного процесса: точное совпадение `CLAUDE_PID` с более близким ancestor `claude`/`node` имеет приоритет над более дальним именованным `claude`; ближайший именованный `claude` остаётся fallback при missing/stale env. Родитель после own ищется только среди `claude(.exe)`. Это исправляет mixed chain, не притворяясь, что npm-only parent решён.
2. Для каждого `SubagentStop` с непустым type требовать непустой `agent_transcript_path` и наличие самого `.jsonl` либо sibling `.meta.json`. Missing path и missing files означают internal/malformed event и дают `Skip`.
3. Разделить timeout: 500 мс для `SessionStart`/`SessionEnd`, 300 мс для остальных событий, которые непосредственно задерживают ввод/agent loop. Один invocation всё равно делает не более одного POST и никогда не retry.

Публичный wire contract, hub registry и ingress не менять. Новых crates кроме target-specific `windows-sys` не добавлять. Shell-form snippet, PostToolUse-only matcher, отсутствие ancestor creation-time validation и двойное хранение shared secret оставить согласно `OPEN_DECISIONS.md`.

## 4. Revised steps

1. **Подготовить baseline без слепого доверия к patch/hash.**
   - Проверить `git status` и не затрагивать пользовательские изменения.
   - Выполнить `git apply --check maw/tasks/in_progress/TASK-012/scratch/planner/task012.patch`.
   - Патч можно применить как механический baseline, но старый `verify_hashes.sh` использовать только для подтверждения исходного reference, не как финальный acceptance gate.
   - После исправлений сверять итог через diff, tests, clippy и fmt; не утверждать, что итог равен старым 18 hash.

2. **Добавить минимальную Windows dependency.**
   - В `crates/cctg/Cargo.toml` добавить target-specific `windows-sys = 0.61` только с `Win32_Foundation` и `Win32_System_Diagnostics_ToolHelp`.
   - В `Cargo.lock` должна добавиться только прямая зависимость `windows-sys 0.61.2` у package `cctg`; новых package downloads нет.

3. **Реализовать device config и cwd normalization в `crates/cctg/src/device.rs`.**
   - `DeviceConfig { secret, hook_addr, host }`; process env → fallback `~/.cctg/device.env`.
   - Читать dotenv через `from_path_iter` в память; никогда не вызывать `set_var`; `dotenvy::Error` не сохранять и не форматировать.
   - Все config errors имеют фиксированный текст без значений; `Secret` остаётся redacted в `Debug`.
   - `canonical_cwd` делает `canonicalize`, fallback на исходную строку и снимает только поддержанные Windows verbatim prefixes.
   - Unit tests: приоритет env, defaults, missing/broken file, отсутствие изменения process env, redaction, relative/dotted path, missing path fallback, Windows case/short-name behavior и UNC prefix.

4. **Реализовать process snapshot в `crates/cctg/src/proctree.rs`.**
   - Windows: один `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)`, `PROCESSENTRY32W.dwSize`, `Process32FirstW`/`Process32NextW`, один документированный unsafe region; каждый валидный handle закрыть ровно один раз через `CloseHandle`.
   - Linux: `/proc/<pid>/stat`, `comm` между первой `(` и последней `)`. Остальные OS возвращают отсутствие chain.
   - Общий обход: максимум 64 узла, stop на missing pid, `ppid=0`, self-parent или cycle.
   - Выбор own:
     - найти ближайший именованный `claude(.exe)`;
     - найти ancestor с pid=`CLAUDE_PID`, но считать его name fallback только если это `claude(.exe)` или `node(.exe)` и он находится ближе, чем первый именованный `claude`;
     - иначе использовать первый именованный `claude`; если chain ничего не дал, сохранить env pid как последний fallback для `SessionEnd`.
   - Parent — следующий именованный `claude(.exe)` выше own и обязательно с другим pid. Если env session id реально отличается от stdin session id и tree parent не найден, сохранить существующий rule-1 fallback на env pid, не делая own своим parent.
   - Tests:
     - TASK-003 B/D/E native chains: top-level, nested, interactive nested-unknown;
     - missing и stale `CLAUDE_PID`, включая stale pid, совпавший с более дальним wrapper;
     - truncated chain возвращает top-level и документирует известную дыру;
     - native own/native parent;
     - mixed node-own/native-parent: own берётся из env pid, parent — из следующего `claude`;
     - npm-only node-own/node-parent: own pid корректен, parent остаётся `None` с явным комментарием «unsupported by project deployment», а не с ложным утверждением о покрытии;
     - cycle/depth cap, case-insensitive names и `/proc/stat` parser.

5. **Расширить `crates/cctg/src/hook.rs`, не меняя transport.**
   - Сохранить `post`, `parse_status`, `PostError` и их TASK-010 tests.
   - Добавить `LIFECYCLE_POST_TIMEOUT = 500 ms`, `BLOCKING_POST_TIMEOUT = 300 ms`, `STDIN_TIMEOUT = 300 ms`, `MAX_STDIN = 8 MiB`, `MAX_TEXT = 128 KiB`.
   - `post_timeout(event)` возвращает 500 мс только для `SessionStart`/`SessionEnd`, 300 мс для остальных поддержанных входов.
   - `read_stdin`: отдельный OS thread, `take(MAX_STDIN + 1)`, bounded receive; timeout/oversize даёт fixed stderr и возврат. Никакого stdout.
   - Узкий `Input`/`ToolInput` с `#[serde(default)]`; parse errors превращать только в static `Skip`.
   - `build(event, input, probe)`:
     - требует непустой `session_id`; при присутствующем `hook_event_name` требует совпадение с CLI event;
     - `SessionStart`: `source`, own pid, parent pid;
     - `SessionEnd`: `reason`, только own pid;
     - `UserPromptSubmit`: только `prompt_id`, никогда `prompt`;
     - `Stop`: `prompt_id`, capped `last_assistant_message`;
     - `SubagentStart`: непустые `agent_id`/`agent_type`;
     - `SubagentStop`: непустые `agent_id`/`agent_type`/`agent_transcript_path`, затем `.jsonl OR .meta.json` existence; иначе Skip; передать path, type, id и capped last message;
     - `PreToolUse`/`PostToolUse`: только `tool_name == SubagentHandback`, непустой agent id и message;
     - unknown/misrouted events: Skip без POST.
   - `cap_text` режет по UTF-8 char boundary и добавляет `…`; serialized normal payload остаётся ниже `MAX_HOOK_BODY`.
   - Все stderr messages фиксированы; не форматировать stdin, addresses, secret или serde error.

6. **Подключить hook CLI в `crates/cctg/src/main.rs`.**
   - В ветке `Command::Hook` поставить fixed panic hook, запустить `hook::run` внутри `tokio::spawn`, игнорировать join result и завершить `std::process::exit(0)`.
   - Это гарантирует exit 0 даже при panic и не ждёт зависший stdin thread при shutdown runtime.
   - `Command::Hub` и `Command::Agent` не менять сверх разделения match arms.

7. **Экспортировать только нужные модули.**
   - В `crates/cctg/src/lib.rs` добавить `pub mod device;` и `pub mod proctree;`.
   - Wire enum, ingress и registry не расширять и не рефакторить.

8. **Добавить обезличенные fixtures.**
   - Девять JSON fixtures под `crates/cctg/tests/fixtures/hook/`: реальные redacted captures TASK-003 для Start/End/Stop/Subagent*/handback и синтетический documented `UserPromptSubmit`.
   - Не включать секреты, user ids, реальные home paths или prompt text, кроме явного synthetic marker, который тест обязан доказать отсутствующим в POST/logs.

9. **Добавить settings snippet `docs/hook-settings.json`.**
   - User-scope content для `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop` и `PostToolUse` с matcher `SubagentHandback`.
   - Оставить shell form `cctg hook <Event>` согласно решению orchestrator-а; `PreToolUse` не регистрировать, но binary продолжает его принимать.
   - Не добавлять secrets, env names, IP, home/project paths или реальные изменения в `.claude/settings.json`.

10. **Расширить unit и integration tests так, чтобы каждый acceptance criterion имел прямое доказательство.**
    - Exact payload shape для шести событий и handback: верхние keys и event-specific keys, отсутствие prompt/background/unrequested source.
    - `source` отсутствует без ошибки у SessionStart и игнорируется у остальных.
    - Broken/empty/truncated/wrong-type/oversized stdin: exit 0, no panic, empty stdout.
    - Открытый stdin, который не пишет и не закрывается: процесс завершается примерно после `STDIN_TIMEOUT`, exit 0, empty stdout, fixed stderr.
    - Для каждого из шести событий и handback: success exit, empty stdout; stderr не содержит secret, полного input и уникальных markers (`session_id`, prompt/report/message fragments, paths).
    - Silent/slow hub: `SessionEnd` <1.2 с; black-hole behavior моделировать локальным listener, который принимает соединение и не отвечает, без внешней сети.
    - Closed port: Start/End exit 0 и <1.2 с.
    - Blocking-event timeout: `UserPromptSubmit`/handback упираются в 300 мс, а fake hub с контролируемой задержкой существенно меньше 300 мс всё ещё принимает событие.
    - Process tree: полный набор из шага 4, включая corrected mixed chain и честно unsupported npm-only parent.
    - Internal agent filtering: empty type; typed stop без path; typed stop с nonexistent files; только jsonl; только meta; настоящий capture. Не требовать памяти о SubagentStart.
    - Cwd: success, fallback и prefix behavior.
    - Settings: ровно семь event keys, только PostToolUse matcher, правильные команды, отсутствие secret/env/path/address tokens.

11. **Проверить итог последовательно, с одним cargo-процессом и target вне repo.**
    ```powershell
    $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task012-final-target'
    cargo test --workspace --offline -j 2
    cargo clippy -p cctg --all-targets --offline -- -D warnings
    cargo fmt --all -- --check
    ```
    - Дополнительно выполнить `git diff --check` и просмотреть итоговый diff: изменены только 18 task-файлов (либо меньше, если fixtures объединены иначе), без `target/`, `.env`, `.cctg/` или реальных settings.
    - Старый `hashes.txt` не использовать как финальный oracle после исправлений; при необходимости воспроизводимого artifact обновить patch/hashes отдельно уже из исправленного дерева.

### Acceptance mapping

| Критерий | Прямое доказательство |
|---|---|
| Шесть payloads, только нужные поля | exact-shape unit tests + integration delivery |
| Unavailable hub: exit 0, empty stdout, safe stderr | closed/silent integration tests для lifecycle и all-event leak assertions |
| Top-level/nested contract | TASK-003 native chain tests + hub semantics; mixed-chain regression |
| Broken/empty/truncated stdin | unit prefix fuzz + CLI malformed/oversized/hung-stdin tests |
| SessionEnd <1.5 с | silent listener wall-clock <1.2 с при lifecycle timeout 500 мс |
| Settings all events, no secrets/paths | parsed snippet test |
| Handback + SubagentStop + internal filter | Pre/Post build tests, Post-only snippet test, missing-path/files cases |
| `source` only SessionStart | dedicated unit test |
| Canonical cwd + fallback | device unit tests + payload assertion |
| Parent pid only for real recognized ancestor | native/mixed/stale/missing/truncated process-tree tests |
| SessionEnd own pid | native and mixed regression tests |
| Existing behavior | full workspace tests, clippy, fmt |

## 5. Risk areas

- **Оборванная process chain.** Умерший wrapper делает nested запуск неотличимым от top-level. Это зафиксированная TASK-003 дыра; без нового межпроцессного протокола текущая задача её не решает.
- **PID reuse.** Creation-time validation отложена решением orchestrator-а. Новый приоритет matching `CLAUDE_PID` исправляет mixed node/native chain, но теоретически stale pid, переиспользованный более близким `node.exe`, может дать ложный own. Тестировать ближайший реалистичный stale-wrapper case и оставить риск явным.
- **npm-only Claude.** Собственный pid можно сохранить по `CLAUDE_PID`, но второго `node.exe` нельзя безопасно объявить Claude-parent только по image name. Проект не разворачивается на Node; не добавлять command-line inspection/`OpenProcess` только ради неподдерживаемой установки.
- **Fire-and-forget теряет событие при timeout.** Retry отсутствует намеренно. 300 мс уменьшает UI-задержку blocking hooks, но повышает шанс потери на очень медленной Tailscale-сети; lifecycle events сохраняют 500 мс. Если реальные метрики покажут RTT >300 мс, timeout должен стать device-configurable отдельной задачей, не через бесконечный retry.
- **Общий SessionEnd budget.** Другие пользовательские SessionEnd hooks делят те же 1.5 с. Наш wall time <1.2 с является верхней границей для cctg, но не гарантирует работу чужих hooks; snippet не должен повышать общий budget.
- **Файловый фильтр internal agents.** Он подтверждён TASK-003 captures и соответствует документированному `agent_transcript_path`, но остаётся эвристикой для недокументированных internal events. Требование path/files может потерять настоящий stop при изменении Claude Code; потеря лучше ложной регистрации internal agent, а `SubagentStart` уже даёт начало жизненного цикла.
- **Shell form и PATH.** Решение сохраняет Git Bash/PowerShell startup и требует `cctg` в PATH. Exec form официально поддерживается для `.exe`, но orchestrator явно выбрал shell form; не переоткрывать решение в TASK-012.
- **`process::exit(0)`.** Оно намеренно обходит destructors/runtime shutdown, чтобы не зависнуть на stdin thread. До него не должно быть buffered stdout или обязательной durable записи.
- **Большие тексты.** `last_assistant_message` и handback capped до 128 KiB; полный ответ остаётся в transcript. Остальные строки приходят из Claude Code и обычно малы; oversized/malicious stdin всё равно обязан завершаться безопасно.
- **Канонизация network/subst paths.** `canonicalize` может превратить drive spelling в UNC. Это допустимо для identity, но меняет отображаемое имя последнего компонента; fallback обязан сохранять исходный путь без ошибки.

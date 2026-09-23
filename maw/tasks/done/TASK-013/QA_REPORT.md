# QA_REPORT — TASK-013 (qa, claude/opus, medium)

**Verdict: NEEDS_FIXES.** Всё по спеке работает, включая живой прогон с настоящим Claude Code 2.1.280. Но фикс ревью внёс регрессию: id разрешений, на которые ответили в терминале, никогда не закрываются. После 64 таких промптов агент до конца жизни процесса (а процесс переживает `/clear`) перестаёт пересылать permission requests в hub.

## Disconfirmation

Контрпример до ревью: «после фикса пользователь, который отвечает на промпты разрешений в терминале, со временем теряет relay в Telegram». Официальный channels-reference (строка "If someone at the terminal answers before the remote verdict arrives, that answer is applied instead and the pending remote request is dropped") говорит, что Claude Code не сообщает серверу о локальном ответе. Значит, в `open_permissions` (`crates/cctg/src/channel.rs:251-281`) id удаляется только при verdict из hub (`channel.rs:183-192`), а при 64 открытых новые запросы отбрасываются (`channel.rs:255-261`).

**Контрпример подтвердился** на настоящем бинарнике: `scratch/qa/perm_leak.py`, результат в `scratch/qa/perm_leak.out.txt`. Из 70 разных запросов без verdict (ответ в терминале) до hub дошли 64, а 65-й..70-й не дошли. В stderr одна строка `too many open permission requests`, Claude об этом не узнаёт.

## 1. Environment

- Прямой прогон, без docker. `CARGO_TARGET_DIR=%LOCALAPPDATA%\Temp\cctg-qa013-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `--offline -j 2`, сборки строго по одной.
- Живой прогон: копия `cctg.exe` в `%TEMP%\cctg13_qa`, временный `mcp.json` (сервер `cctgqa`, синтетический секрет, `CCTG_HUB_AGENT_ADDR=127.0.0.1:47491`, `CCTG_HOST=qa13`), `scratch/planner/fake_hub.py 47491 ...` как hub, `scratch/qa/run_interactive_scenario.py` (копия скрипта планировщика, env без `CLAUDE*`) запускает `claude --mcp-config <tmp>\mcp.json --dangerously-load-development-channels server:cctgqa --model haiku --debug-file <tmp>\debug_QA2.log` в отдельной консоли. `~/.claude.json` и `.env` не трогались, Telegram API не вызывался.
- Воспроизвести:
  ```
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --offline -- -D warnings
  cargo test --workspace --offline
  python maw/tasks/in_progress/TASK-013/scratch/qa/perm_leak.py <target>/debug/cctg.exe 70
  ```
- Уборка: оба fake hub остановлены, процессов с `fake_hub.py 47491` и `cctg13_qa` нет (проверено по командной строке). Сессии claude убил сам скрипт (`taskkill /T` только по pid, который он запустил). Удалены `%TEMP%\cctg13_qa` и `~/.claude/projects/C--Users-user-AppData-Local-Temp-cctg13-qa`. Осталось на стороне пользователя: ключ `projects[...Temp\cctg13_qa]` в `~/.claude.json`, его создал сам Claude Code (как и в TASK-004).

## 2. Test results

| Проверка | Результат |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, без предупреждений |
| `cargo test --workspace`, 3 прогона подряд | каждый раз 315 passed, 0 failed, 1 ignored. Флейков нет |
| `scratch/qa/perm_leak.py` (новый, настоящий бинарник + минимальный hub, `RUST_LOG=trace`) | 64 из 70 relayed, **6 потеряны**. stdout 1 строка, валидный JSON, exit 0, секрета нет ни в stdout, ни в stderr |
| `cctg agent-install` | `claude mcp add --scope user cctg -- "C:\Users\user\AppData\Local\Temp\cctg13_qa\cctg.exe" agent`: абсолютный путь, без `\\?\`, ничего не выполняет |
| Живой E2E, прогон 1 (`scratch/qa/qa_hub_run1.jsonl`, `screen_QA_*.txt`) | Register и inbound A/B дошли (до и после `/clear`), но Haiku не нашёл deferred tool `reply` и ответил в терминал. Это поведение модели, не агента |
| Живой E2E, прогон 2 (`scratch/qa/qa_hub.jsonl`, `live_evidence.txt`, `screen_QA2_*.txt`) | всё прошло, подробности ниже |

Живой прогон 2 (Claude Code 2.1.280, pid 40348):
- `register {session_id: 9068257b…, host: "qa13", cwd: "C:\Users\user\AppData\Local\Temp\cctg13_qa", claude_pid: 40348}`. `claude_pid` равен pid запущенного claude, cwd канонический, хост из `CCTG_HOST`.
- В debug-логе есть `Channel notifications registered`, согласованная версия `2025-11-25`.
- Inbound A дошёл: `notifications/claude/channel: QA13-A…`, тег в транскрипте сессии 9068257b.
- Permission relay: `permission_request yfsgn` (mcp__cctgqa__reply) пришёл в hub, `allow` вернулся, в debug-логе `yfsgn → allow (matched pending)`. После этого `reply {text: "pong-a"}` пришёл в hub.
- `/clear`: MCP-сервер не перезапустился (одно соединение с hub за весь прогон, env id остался старым). Inbound B дошёл в новую сессию: транскрипты после clear содержат QA13-B, транскрипт 9068257b его не содержит. Второй relay (`hbiot`) и `reply pong-b` тоже прошли.
- Перепривязку на стороне hub по `(host, claude_pid)` живьём не проверял: fake hub её не делает. Её покрывают `slots::tests::the_agent_follows_its_claude_process_through_clear` и `…_when_the_new_start_comes_first`, код `slots.rs` `agent_session`/`follow_pid` я прочитал.

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| initialize → initialized → tools/list → tools/call, по одному JSON-объекту на строку | `agent_stdio` (4 процессных теста), живой прогон, `perm_leak.py` | PASS |
| неизвестный метод `-32601`, битый ввод контролируем | unit-тесты `channel::tests::{bad_calls…, broken_input…, only_json_rpc_2_0…}` (каждый префикс запроса, не-UTF-8, oversized line), код `on_line` прочитан | PASS |
| stdout без логов и паник, в т.ч. при недоступном hub | `agent_stdio::an_unreachable_listener…`, `…version_rejection…`, `perm_leak.py` при `RUST_LOG=trace` (warn реально пишется, stdout чистый). Panic hook: `main.rs` ставит hook, который пишет фиксированную строку через `write_panic_message` и не трогает payload; `tokio::spawn` ловит панику задачи, затем `exit(0)`. Настоящую панику в проде вызвать нечем, проверено чтением кода и unit-тестом | PASS (panic только по коду) |
| невалидный ключ meta отброшен, валидные байт в байт | unit-тест с кириллицей, эмодзи, `\n`, кавычками. В живом прогоне fake hub шлёт `bad-key`, но тег в транскрипте я не сохранил до удаления каталога, так что живьём это не подтверждено | PASS |
| разрыв и восстановление hub, перерегистрация той же сессии | `agent::tests::the_channel_relays_both_ways_and_survives_a_hub_restart` (тот же `Register`, очередь reply доставлена) | PASS |
| ручной запуск: баннер и inbound; вложенный запуск не маршрутизируется | живой прогон 2 (registered + inbound). Вложенный запуск: `sdk-cli` → `NoHub::Headless`, без соединения (`agent_stdio::headless…`, `link_plan_rules`), на hub `agent_connected` не связывает Nested (`registry::tests::the_agent_of_a_nested_run_is_never_bound`). Живьём вложенный запуск не повторял, у планировщика он есть в `live_hub.jsonl` | PASS |
| cwd и хост тем же хелпером, что у хука | `run_stdio`: `config.host` из `DeviceConfig` и `device::canonical_cwd`. В живом прогоне cwd канонический | PASS |
| `/clear` проверен наблюдением, перерегистрация | живой прогон 2: сервер не перезапускается, inbound и relay доходят в пост-clear сессию. Перепривязка на hub через unit-тесты slots | PASS |
| Existing tests pass | 3×315/0/1 | PASS |
| (фикс ревью) relayed id не вытесняется, verdict ровно один раз, без повторного relay | unit-тест `permission_capacity_never_forgets_an_open_relayed_id` проходит, но из-за этой политики id никогда не закрываются: см. Bug 1 | **FAIL (регрессия)** |
| (фикс ревью) недоступный и отклоняющий hub | процессные тесты проходят. Отклонение `auth` у fake hub: секрет в stderr не попадает | PASS |
| (фикс ревью) кавычки в agent-install в зависимости от платформы | Windows `"…"`, POSIX `'…'` с `'\''` (тест только `cfg(not(windows))`, здесь не запускался, код прочитан) | PASS, см. Nit |

## 4. Bugs found

### Bug 1 — Major: id разрешений, отвеченных в терминале, копятся, relay навсегда умирает на 65-м

- Где: `crates/cctg/src/channel.rs:251-281` (добавление и отказ по capacity), `:183-192` (единственное место, где id закрывается).
- Причина: Claude Code не шлёт серверу ничего, когда на промпт ответили в терминале (channels-reference: "the pending remote request is dropped"). Hub тоже не шлёт verdict для промптов, на которые ответили локально, а после рестарта hub забывает старые. То же с запросом, чья запись в hub-линк упала ("a message whose write failed is lost"). Все такие id остаются в `open_permissions` навсегда. Агент живёт столько же, сколько процесс claude, включая все `/clear`.
- Воспроизведение: `python scratch/qa/perm_leak.py <cctg.exe> 70`.
- Ожидалось: каждый новый permission request доходит до hub, пока линк жив.
- Фактически: после 64 промптов, отвеченных в терминале, ни один новый не пересылается, до перезапуска Claude Code. Одна warn-строка в stderr (её видно только в debug-логе Claude Code), пользователь в Telegram просто перестаёт получать кнопки.
- Почему фикс неверен: исходную находку ревью ("verdict для вытесненного id отбрасывается") переоценили. Claude Code сам принимает verdict только для id, который он выдал и который ещё pending ("matched pending" в debug-логе), так что лишний verdict ничего не ломает. А отказ от вытеснения ломает реальный сценарий.
- Предложение: вернуть ограниченное FIFO/LRU-вытеснение самого старого id (он почти наверняка уже решён в терминале) или вообще не фильтровать verdict по открытым id и оставить только небольшое окно дедупликации запросов. Тест: N > 64 запросов без verdict, каждый новый доходит до hub.

### Nit — `agent-install` на Windows печатает путь в двойных кавычках

Если команду вставить в PowerShell, а в пути есть `$` или обратная кавычка, путь исказится (`"C:\Users\$x\..."` раскроется). На Windows такое редко, поэтому это не блокер. Вариант: одинарные кавычки для PowerShell или пометка «выполнить в cmd».

### Замечание (не баг агента)

Инструмент `reply` в Claude Code 2.1.280 deferred. В прогоне 1 Haiku не нашёл его по фразе "reply tool" и ответил в терминал. `INSTRUCTIONS` можно усилить: назвать полное имя `mcp__<server>__reply` или написать «load it with ToolSearch if it is not listed». Это к рассмотрению, агент тут не нарушает контракт.

## 5. Утечки секрета и данных

- stderr агента: только фиксированные тексты. `ConfigProblem` не содержит значений, `NoHub::text` фиксирован, ошибки линка это TASK-010 с фиксированными текстами. Содержимое inbound/reply/permission в логах не встречается: у `channel_meta` только счётчик, у `on_line` только фиксированные тексты ошибок.
- Живой прогон: синтетического секрета нет ни в одном файле `scratch/qa/` (кроме самого скрипта `perm_leak.py` с собственным синтетическим значением), в debug-логе его тоже нет. email аккаунта из снимков экрана заменён на `<email>`.
- Логирование payload в stderr/ошибки не найдено.

## 6. Verdict

**NEEDS_FIXES.** Протокол, чистота stdout, meta, переподключение, `/clear` и живой relay работают и подтверждены независимо. Блокирует одна вещь: политика «никогда не забывать relayed id» (Bug 1), внесённая фиксером. Она гарантированно отключает permission relay в долгой интерактивной сессии, где пользователь часть промптов отвечает в терминале. Это как раз основной сценарий cctg. Исправление локальное (`channel.rs`, одна структура и один тест).

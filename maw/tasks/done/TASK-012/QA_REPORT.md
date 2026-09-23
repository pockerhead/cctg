# QA REPORT — TASK-012 (`cctg hook`)

Проверяемый код: ветка `feature/hook-lifecycle`, HEAD `144ee6d` (фикс `3acc796`), `git diff main` затрагивает 18 файлов в `Cargo.*`, `crates/`, `docs/`. Код не менялся.

## 0. Disconfirmation

Контрпример, записанный до проверки: "после фикса (родитель обязан быть образом Claude Code CLI и строго старше ребёнка) обход на живом Windows-дереве либо (а) объявит настоящую top-level сессию вложенной из-за Claude Desktop `claude.exe` выше неё, либо (б) оборвёт цепочку на проверке creation time (грубое разрешение времени, родитель и ребёнок в одном тике) и потеряет настоящего родителя у вложенного запуска".

Что проверено:
- Живое дерево этого хоста (только pid, имя образа, класс пути, относительный возраст). 13 процессов `claude.exe` Claude Desktop (Store, `WindowsApps\Claude_*`) и 4 CLI `claude.exe` (`.local\bin`). Ни у одной CLI-сессии нет claude-предка: цепочки заканчиваются на `bash`/`powershell` → `Cursor.exe` → `explorer.exe` или на мёртвом pid. Ни одна CLI-сессия сейчас не запущена из Desktop. Время создания у процессов идёт с микросекундной точностью. Во всех 7-звенных цепочках родитель строго старше ребёнка, одинакового времени нет.
- Настоящий релизный хук через Git Bash под этой сессией (`bash -c "cctg hook SessionStart"`) отправил `claude_pid = 36120` (равен `CLAUDE_PID` сессии) и `parent_claude_pid: null`. Сессия правильно распознана как top-level.
- Вложенность на живых процессах: поддельный `claude.exe` (копия `cmd.exe`) запускался прямо из этой сессии. Хук вернул `claude_pid = <поддельный>`, `parent_claude_pid = 36120`: цепочка из 7 звеньев прошла проверку creation time. С двумя поддельными уровнями родителем стал ближний поддельный, а не 36120.

**Контрпример не подтвердился.** По ходу нашёлся соседний случай: он воспроизводит уже известную дыру TASK-003 и регрессией не является (bug 2).

## 1. Environment

- Прямой запуск, без docker и без dev-сервера. В репо нет compose и нет dev-таргета.
- `CARGO_TARGET_DIR=%LOCALAPPDATA%\Temp\cctg-qa012-target` (вне репо), одна сборка за раз, `--offline -j 2`.
- Hub заменён QA-заглушкой `scratch/qa/fakehub.py`. Режим `capture` отвечает `204` и сохраняет только тело POST, без заголовков. Режим `blackhole` принимает соединение и молчит. Для режима "hub нет" берётся закрытый порт. Настоящий `ingress::serve_hooks` проверяется интеграционными тестами `hook_cli`.
- Конфиг устройства: временный `HOME`/`USERPROFILE` в `scratch/qa/home` с фиктивным секретом, после прогона удалён. Реальные `.env` и `~/.cctg/device.env` не читались. Хуки ни в какие settings не регистрировались. Telegram не вызывался.
- Скрипты QA лежат в `scratch/qa/`: `fakehub.py`, `timing.py` (замер через `C:\Program Files\Git\bin\bash.exe -c`), `chain.ps1` (дамп цепочки предков без путей), `show.py`, `lineage_probe/` (отдельный crate с синтетическими тестами `cctg::proctree::lineage`).

Воспроизведение:
```bash
export CARGO_TARGET_DIR="$LOCALAPPDATA/Temp/cctg-qa012-target"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -j 2 -- -D warnings
cargo test --workspace --offline -j 2            # x3
cargo test -p cctg --test hook_cli --offline -j 2  # x8
cargo build -p cctg --release --offline -j 2
cd maw/tasks/in_progress/TASK-012/scratch/qa/lineage_probe && cargo test --offline -j 2
python timing.py <release cctg.exe> <win home dir with .cctg> blackhole SessionEnd end.json 20
```

Запущенные сервисы: только `python fakehub.py` / `timing.py` с ограниченным временем жизни (30–40 с). Все завершились, живых процессов нет.

## 2. Test results

Существующие и новые тесты репозитория:
- `cargo fmt --all -- --check`: чисто.
- `cargo clippy --workspace --all-targets -- -D warnings`: чисто.
- `cargo test --workspace`: 3 прогона подряд, каждый дал 280 passed, 0 failed, 1 ignored. Ignored это старый `hub::config` тест, который запускается в изолированном процессе.
- `hook_cli`: 8 прогонов подряд, 8/8 по 6 passed, около 1.07 с. Флаков нет.

Новые QA-проверки (сценарии и тесты свои, скрипты авторов не запускались):

| # | Проверка | Результат |
|---|---|---|
| Q1 | `lineage_probe`, 6 синтетических цепочек: bundled CLI под Store Desktop (с env pid, указывающим на Desktop, и с чужим env session), Desktop-путь в верхнем регистре и с `/`, classic `AnthropicClaude`, настоящий CLI-родитель выше Desktop-hosted цепочки, Desktop между двумя CLI, усечённая цепочка без claude, Linux-имена без путей | 6/6 pass |
| Q2 | Живой top-level через Git Bash: SessionStart и SessionEnd | own = `CLAUDE_PID` сессии, parent `null`, exit 0, stdout пуст, stderr пуст |
| Q3 | Живой nested (поддельный claude без env / с устаревшим `CLAUDE_PID` = внешний / два уровня) | parent = настоящий внешний claude; при двух уровнях parent = ближний. Проверка creation time на реальных процессах проходит |
| Q4 | Живой SessionEnd под поддельным claude с `CLAUDE_PID=99999` | `claude_pid` = pid поддельного claude из обхода, не из env |
| Q5 | SessionEnd, hub принимает и молчит, через Git Bash, 20 прогонов | min 543 / median 556 / **max 576 мс** |
| Q6 | SessionEnd, порт закрыт, 20 прогонов | min 543 / median 556 / max 573 мс (Windows повторяет SYN до таймаута) |
| Q7 | SessionEnd на немаршрутизируемый `10.255.255.1`, несуществующее имя `.invalid`, `[::1]:1`, адрес без порта | 553 / 63 / 566 / 44 мс по медиане, в stderr нет адреса |
| Q8 | UserPromptSubmit, hub молчит / закрыт | median 342 / 344 мс (таймаут 300 мс), текст промпта в теле POST отсутствует |
| Q9 | Открытый и молчащий stdin | median 348 мс, exit 0, одна фиксированная строка |
| Q10 | stdin 9 MiB, случайные байты, `/dev/null`, битый JSON | exit 0, stdout пуст. Для 9 MiB одна фиксированная строка, для остальных тишина (skip на debug) |
| Q11 | `RUST_LOG=trace` | уровень не меняется, лишнего вывода нет |
| Q12 | Приоритет device.env: пустые и пробельные `CCTG_*` в process env; process host/addr/secret против файла; конфиг только в process env; конфиг не задан | пустые значения не затеняют файл; непустые значения из process env побеждают, в том числе плохой секрет (`BadSecret`); без конфига фиксированная строка `NoSecret` |
| Q13 | Матрица событий через CLI: SubagentStop с пустым типом / с типом без файлов / настоящий / только `.meta.json`; SubagentStart пустой / настоящий; PostToolUse Bash / handback; Stop; UserPromptSubmit; misrouted | до hub доходят только настоящие события; поля точные, `background_tasks` и `prompt` не уходят |
| Q14 | Во всех прогонах Q2–Q13 stderr проверялся на секрет, session id, `reason`, cwd, `Users`, порт, ANSI и timestamp | утечек нет, ANSI нет, timestamp нет. Строки вида ` WARN hook event not delivered event="session_end" error=hub did not answer within 500ms` |
| Q15 | Grep по diff и фикстурам: `C:\Users\user`, `C--Users`, email, токены бота, user id | ничего не найдено |
| Q16 | Grep по `hook.rs`/`device.rs`/`proctree.rs`/`main.rs` на `println!`/`dbg!`/`unwrap` в runtime-путях | в runtime только `expect` на `serde_json::to_vec(HookPost)` (недостижим) и фиксированный panic hook |

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| Каждое из шести событий даёт ожидаемый пейлоад, без лишних полей | unit `each_event_carries_its_fields_and_nothing_else` (точные множества ключей), `hook_cli::every_event_reaches_the_hub` (настоящий ingress), Q13 через CLI | PASS |
| Недоступный hub: exit 0 в пределах таймаута, stdout пуст, в stderr нет пейлоада и секретов | Q5–Q7, Q14, `hook_cli` | PASS |
| Вложенность и parent id по правилу TASK-003 | unit-тесты `proctree` (цепочки B/D/E TASK-003), Q1, живые Q2–Q3 | PASS (известная дыра с оборванной цепочкой остаётся, см. bug 2) |
| Битый / пустой / обрезанный stdin: exit 0 без паники | `broken_input_is_skipped_without_panicking` (все префиксы), Q10, Q9 | PASS |
| SessionEnd измерен и заведомо укладывается в 1.5 с при недоступном hub | Q5/Q6: максимум 576 мс через Git Bash, релизная сборка | PASS |
| Snippet регистрирует все события, без секретов и машинных путей | `docs/hook-settings.json` прочитан глазами (7 ключей, `cctg hook <Event>`, matcher `SubagentHandback` только на PostToolUse), тест `settings_snippet_…` | PASS |
| Handback через matcher; SubagentStop несёт `agent_transcript_path`, `agent_type`, `agent_id`, `last_assistant_message`; внутренние отбрасываются | Q13, unit `internal_agents_are_dropped` | PASS с оговоркой: typed `SubagentStart` для `--agent` сессии не фильтруется. Это задокументированный остаточный риск, полями payload его не различить. Для Stop фильтр по наличию файлов субагента это прокси, живьём на `--agent` сессии не проверен |
| `source` только из SessionStart, необязателен | `source_is_optional_and_read_only_from_session_start`, код `hook.rs:174-181` | PASS |
| `cwd` канонизирован на устройстве, без `\\?\`, fallback при ошибке | Q2 (вариант `c:\users\user\DEV\cctg\.` дал тот же канонический путь), `device::tests` | PASS |
| `parent_claude_pid` только при claude-предке, отличном от своего; top-level шлёт `None` | Q2 (None на реальном дереве с живым Desktop), Q3, Q1 | PASS |
| SessionEnd несёт собственный `claude_pid` из обхода дерева | Q4 (env подменён, ушёл pid из обхода), Q2 | PASS |
| Existing tests pass | 3 прогона workspace | PASS |

Проверка заявлений фиксера по коду:
- Путь Desktop: `is_claude_desktop_path` (`proctree.rs:179-182`) это case-insensitive проверка подстрок `\windowsapps\claude_` и `\anthropicclaude\`, `/` нормализуется. На хосте в Store-пакете есть ровно один `claude.exe` (`\app\claude.exe`), bundled CLI лежит в `%APPDATA%\Claude\claude-code\2.1.280\`, под правило не попадает. Подтверждено.
- Родитель строго старше, обход стоп на первом непроверенном звене: `ProcessTable::chain` (`proctree.rs:62-90`) и `windows::process_table` (break при неудачном `query_process`). Подтверждено кодом, unit-тестом и живыми цепочками.
- Непроверенный `CLAUDE_PID` не используется как родитель: fallback фильтруется `ancestors…is_some_and(is_cli_claude)` (`proctree.rs:148-155`), Q1. Подтверждено.
- RAII на хэндлах: `OwnedHandle` закрывает хэндл один раз, raw-хэндл проверяется до обёртки. Подтверждено.
- Пустой process env не затеняет файл: `prefer_non_empty` (`device.rs:94-96`), Q12. Подтверждено.
- stderr хука без ANSI и времени: `init_tracing(true)` (`main.rs:50-57`), Q14. Подтверждено.
- Linux-ветка по-прежнему не скомпилирована (нет target). Не проверено ни фиксером, ни QA.

## 4. Bugs found

Блокирующих дефектов нет.

1. **low** — `cctg hook` без имени события или с лишним аргументом выходит с кодом 2 (ошибка clap, usage в stderr), а не 0.
   Шаги: `cctg hook < end.json` или `cctg hook SessionEnd extra < end.json`.
   Ожидалось: exit 0 на любом пути. Получено: exit 2. Для `UserPromptSubmit` exit 2 у Claude Code значит "заблокировать промпт".
   Срабатывает только при ручной ошибке в регистрации, поставляемый snippet всегда передаёт ровно один аргумент. Неизвестное имя события (`cctg hook PreCompact`) даёт exit 0.

2. **low / известное ограничение, не регрессия** — вложенный запуск через MSYS-exec выглядит top-level.
   Шаги: из Git Bash выполнить `bash -c './claude.exe …'` или `env X=1 ./claude.exe …`, где внутри срабатывает хук SessionStart.
   Ожидалось: `parent_claude_pid` = внешний claude. Получено: `null`. У нативного ребёнка ppid указывает на MSYS-процесс, который сделал exec и уже завершился (`chain.ps1`: `[pid gone]` сразу над ним). Это дыра TASK-003 "короткоживущий wrapper", но она шире, чем там описано: достаточно `bash -c` с одной командой, запущенного из другого MSYS-процесса. В реальных цепочках Bash-тула Claude Code (нативный родитель у первого bash) звенья живые, это подтверждают TASK-003 и Q3. Runner-ы вроде maw, которые оборачивают `claude -p` в `bash -c`/`env`, надо проверить отдельно. Записано в `PCTX_PROPOSALS.md`.

3. **info** — критерий 7 в части "typed `agent_type` = имя `--agent` сессии" закрыт косвенно. Stop отбрасывается по отсутствию файлов субагента, Start не фильтруется. Если `SubagentStart` вообще стреляет для главного потока `--agent` сессии, hub получит фантомного субагента без Stop. Живьём не проверено: нужна настоящая `--agent` сессия с зарегистрированными хуками, а это запрещено scope задачи.

Не проверено в этой задаче: Linux `/proc` ветка (нет target), поведение при живом Claude Desktop, который сам запускает CLI с хуками (сейчас таких процессов на хосте нет, случай покрыт синтетикой Q1), реальный запуск хуков из Claude Code (регистрация запрещена).

## 5. Verdict

**SHIP.**

Все acceptance criteria выполнены. Каждый проверен тестами репозитория, моими синтетическими цепочками и живыми прогонами релизного бинаря через Git Bash на реальном дереве процессов этого хоста. Фикс по Claude Desktop работает, как заявлено: с живым Desktop на хосте ни одна реальная top-level сессия не объявляется вложенной, а настоящая вложенность через живую цепочку находится. SessionEnd при молчащем или отсутствующем hub укладывается в 576 мс при бюджете 1.5 с. Путей утечки секрета, пейлоада или приватных путей в stderr не найдено. Найденное (exit 2 при неправильной регистрации, расширенная известная дыра MSYS-exec, непроверенный фильтр `--agent` для Start) по severity низкое и ни один критерий не ломает. Это стоит взять в задачу установки и в проверку maw runner.

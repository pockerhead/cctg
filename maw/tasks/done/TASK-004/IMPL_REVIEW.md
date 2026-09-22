# Review TASK-004

## Verdict

**NEEDS_WORK** — основные наблюдения о channel lifecycle в целом подтверждаются, но обязательная 4-mode матрица имеет непроверенные клетки, несколько headline-выводов нельзя восстановить из сохранённых raw-артефактов, а cleanup/redaction не доведены до требуемого состояния.

## Проверенный контрпример

Самый конкретный опровергающий случай: строка `--resume` была бы неверна, если бы `probe_log_N3.jsonl` показал новый session id либо не показал две заявленные доставки. Контрпример **не подтвердился**: start-запись содержит прежний id `684d8e75-...`, обе outbound-нотификации привели к двум `permission_request` и двум `tools/call` (`scratch/probe_log_N3.jsonl:1`, `:8-20`). Однако отдельного `debug_N3.log` нет, а сохранённый launcher output не фиксирует, что N3 действительно был запущен именно с `--resume`; это отражено ниже как пробел доказательств.

## Confirmed correct

- Probe соответствует минимальному stdio JSON-RPC контракту: объявляет обе experimental capabilities, реализует `tools/list`, `tools/call`, permission reply и `-32601`, не пишет диагностику в stdout (`scratch/probe_channel_server.py:44-60`, `:112-160`). Повторный `python scratch/selftest_probe.py` дал 8/8 PASS.
- Fresh interactive channel реально работал: баннер сохранён, I5 отправил три inbound, получил три permission requests и три вызова `reply` (`scratch/screen_I5_banner.txt:1`, `scratch/probe_log_I5.jsonl:7-27`).
- N2 и N3 продолжают одну и ту же историю с id `684d8e75-...`; в обоих probe-логах по две доставки и вызова `reply`, а captures показывают прежнюю историю и channel banner (`scratch/probe_log_N2.jsonl:1`, `scratch/probe_log_N3.jsonl:1`, `scratch/screen_N2_banner.txt:1`, `scratch/screen_N3_banner.txt:1`).
- Headless `-p` действительно спавнит probe как обычный connected MCP и публикует `mcp__probe__reply`, но не поднимает channel subsystem: в `debug_M1c.log`/`debug_M2.log` есть successful connection и нет channel registration/skipped, а M1/M1b/M1c/M2 отправили channel notifications без permission/tool-call ответа. `run_M2_stream.jsonl` сохраняет тот же resumed session id и результат `HEADLESSRESUME` (`scratch/FINDINGS.md:78-105`).
- No-flag сценарий подтверждает spawn и silent drop inbound: `debug_N5.log:239,264` содержит successful connection и `Channel notifications skipped`, `probe_log_N5.jsonl` содержит две outbound channel notifications и ни одного permission/tool call, а `screen_N5_banner.txt` не содержит channel banner (`scratch/FINDINGS.md:245-262`).
- Вложенный N4 действительно спавнил второй probe: parent имеет `entry=cli` и id `684d8e75-...`, child — `entry=sdk-cli` и другой id `f26f0436-...`, после чего child получил EOF (`scratch/probe_log_N4.jsonl:1`, `:7-9`).
- Точная MVP-команда, bash/PowerShell aliases и принятое решение «без `cctg run`» записаны явно (`scratch/FINDINGS.md:266-302`).
- `claude mcp list` сейчас не показывает probe, а `~/.claude.json` не содержит `probe_channel_server`; это подтверждает удаление MCP server registration, хотя более строгий cleanup ниже не пройден.
- `cargo clippy --workspace --all-targets -- -D warnings` и `cargo test --workspace` проходят. Запрещённые `rmcp`/`teloxide` не добавлены; production code не менялся.
- Все четыре записи `log.jsonl` имеют UTC timestamp с суффиксом `Z`. Первый `dead_end` подтверждён debug/probe evidence. Второй подтверждает рабочую замену через Win32 API в коде, но исходный отказ AppActivate сохранён только как prose claim.

## Issues

### Major — точные `--continue` и `--resume` invocation не сохранены в raw evidence

**Файл:** `scratch/FINDINGS.md:40`

N2/N3 probe logs и screens доказывают продолжение прежней сессии, но не различают `--continue` и `--resume`: `interactive_N2_session.txt` и `interactive_N3_session.txt` пусты, launcher stdout с точным argv не сохранён, а строки команд существуют только в FINDINGS. Поэтому обязательный headline «оба режима наблюдались» нельзя независимо восстановить из raw captures.

**Suggested fix:** повторить или переоформить evidence так, чтобы для каждого запуска сохранялся launcher stdout с точным argv, exit status, label и временем; связать его с probe log через nonce/scenario.

### Major — `/mcp` снят только для одного fresh-прогона

**Файл:** `scratch/FINDINGS.md:31`

Raw screen с `/mcp` существует только для I4 (`screen_I4_mcp.txt` и связанные I4 captures). Для N2 (`--continue`), N3 (`--resume`) и N5 (без флага) нет `/mcp` captures, хотя таблица заполняет эти клетки как наблюдавшиеся (`scratch/FINDINGS.md:34-36`). Единственный I4 screen нельзя переносить на остальные режимы в spike, где требовалась матрица наблюдений.

**Suggested fix:** сохранить `/mcp` screen в каждом из четырёх режимов либо явно поставить «не наблюдалось» и повторить недостающие прогоны.

### Major — no-flag `permission_request: нет` не был проверен провоцирующим действием

**Файл:** `scratch/FINDINGS.md:36`

В N5 пользователь просил только вывести `NOFLAG`; операция, которая требует permission, не запускалась. Ноль permission requests в `probe_log_N5.jsonl` поэтому не доказывает поведение permission relay без флага. Для сравнения Bash permission в N2 появился только после записи файла.

**Suggested fix:** в no-flag/manual-mode прогоне запросить тот же side-effecting Bash command, сохранить terminal prompt и подтвердить отсутствие channel `permission_request` в probe log.

### Major — вывод о nested run как «не маршрутизируемой регистрации» не доказан и противоречит evidence

**Файл:** `scratch/FINDINGS.md:218`

N4 доказывает второй server process с собственным session id. Сам FINDINGS затем признаёт, что для hub, сопоставляющего по env id, он выглядит самостоятельной сессией и `cctg agent` попытается подключиться (`scratch/FINDINGS.md:221-231`). Отсутствие channel subsystem у `-p` не мешает агенту зарегистрироваться в hub и само по себе не доказывает, что регистрация не станет маршрутизируемой. Утверждение «самостоятельной маршрутизируемой регистрации не возникает» (`:228`) слишком сильное.

**Suggested fix:** сформулировать результат как обнаруженный риск: nested `-p` спавнит отдельный agent с новым id, который hub обязан отклонить/приклеить к parent. Для утверждения об отсутствии самостоятельной регистрации нужен интеграционный capture фактического hub handshake/registry decision.

### Major — precondition «заведомо новая папка отсутствовала в config» существует только в prose

**Файл:** `scratch/FINDINGS.md:181`

Поиск raw artifacts находит фразу `folder already known` только в FINDINGS. `inspect_user_config.py` умеет печатать безопасный результат, но его pre-launch output не сохранён. `screen_N1_startup.txt` подтверждает немедленный banner без видимого consent, но не подтверждает исходное отсутствие folder entry до запуска.

**Suggested fix:** сохранить обезличенный pre-launch output `project_absent=true` и post-launch capture; не выводить остальные config entries.

### Major — cleanup не соответствует заданной read-only проверке

**Файл:** `scratch/FINDINGS.md:360`

Текущая проверка показывает: `claude mcp list` probe не содержит и строка `probe_channel_server` из `~/.claude.json` удалена, но literal grep `probe` по `~/.claude.json` всё ещё даёт одно совпадение. Следовательно, требование reviewer prompt «`~/.claude.json` must not contain `probe`» не выполнено, а cleanup claim неполон.

**Suggested fix:** безопасно определить и удалить только оставшуюся probe-related запись, не затрагивая остальные config entries, затем сохранить лишь boolean/count result повторного grep.

### Major — scratch не обезличен от home path и user name

**Файл:** `scratch/FINDINGS.md:63`

Captures содержат `C:\Users\user`, `C:/Users/user` и encoded `C--Users-user`: точные literal scans нашли такие данные соответственно в 11, 18 и 5 файлах (наборы пересекаются). Это включает FINDINGS, probe logs, stream-json и screen captures. Bot-token shape, Telegram field names и `-100...` supergroup-id shape не найдены, но обязательная проверка absolute home paths/user names провалена.

**Suggested fix:** редактировать все три формы home path/username во всех `*.md`, `*.jsonl`, `*.log`, `*.txt`, затем повторить filename-only scans. Сохранять структуру путей через нейтральные placeholders вроде `<home>` и `<encoded-home>`.

### Minor — fresh row приписывает Bash permission другому сценарию

**Файл:** `scratch/FINDINGS.md:33`

I5 fresh содержит три permission requests только для `mcp__probe__reply`; Bash permission `tcmbm` находится в N2 (`--continue`). Формулировка fresh row «да (Bash и MCP-tool)» не соответствует raw I5 log.

**Suggested fix:** указать для fresh «да (MCP-tool)», а Bash оставить в строке N2, либо приложить отдельный fresh Bash capture.

## Missing coverage

- Exact argv evidence для fresh, `--continue`, `--resume` и no-flag, связанное с nonce каждого probe log.
- Отдельный `/mcp` capture во всех четырёх режимах.
- Side-effecting permission prompt в no-flag режиме.
- Реальный hub handshake/registry decision для nested `claude -p`; текущий spike проверяет только MCP/channel side.
- Сохранённый безопасный pre-launch config check для новой папки.
- Post-cleanup assertions: zero literal `probe` in `~/.claude.json`, zero home/user path variants in scratch, zero bot-token/Telegram-id patterns.

## Nits

- Claim о провале AppActivate/SendKeys (`scratch/FINDINGS.md:344-346`) не имеет raw capture исходной попытки; рабочая замена хорошо видна в коде, но причина замены остаётся только prose/log pointer.
- Claim, что hidden flag отсутствует в `claude --help` (`scratch/FINDINGS.md:304-305`), не подкреплён сохранённым help/grep output. Это не влияет на основные lifecycle-выводы, но его стоит либо снабдить capture, либо пометить как несохранённое наблюдение.

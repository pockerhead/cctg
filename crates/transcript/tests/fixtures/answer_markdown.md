# TASK-004 — IMPL_SUMMARY

Режим: small-fix, спайк. Производственного кода нет по условию задачи
(`crates/`, `.claude/`, `CLAUDE.md`, `.env`, `maw/project-context/` не тронуты).
Основной результат — `scratch/FINDINGS.md`.

## 1. Что сделано

Все файлы в `maw/tasks/in_progress/TASK-004/`.

| файл | строк | что это |
|---|---|---|
| `scratch/FINDINGS.md` | 366 | основной артефакт: таблица 4 режимов и все наблюдения |
| `scratch/probe_channel_server.py` | 166 | probe: stdio JSON-RPC channel-сервер на Python |
| `scratch/selftest_probe.py` | 60 | проверка probe без Claude Code, 8/8 PASS |
| `scratch/run_interactive_scenario.py` | 95 | прогон интерактивной сессии по таймлайну |
| `scratch/type_into_console.py` | 79 | ввод в чужую консоль (AttachConsole + WriteConsoleInputW) |
| `scratch/read_console_screen.py` | 69 | снимок экранного буфера чужой консоли |
| `scratch/launch_interactive_probe.py` | 87 | ранний лаунчер, адаптация из TASK-003 |
| `scratch/inspect_user_config.py` | 31 | безопасный отчёт по `~/.claude.json` |
| `scratch/redact_scratch.py` | 25 | вычистка e-mail из снимков |
| `scratch/append_log.py` | 60 | дозапись в `log.jsonl` |
| `PCTX_PROPOSALS.md` | 55 | 4 предложения в project-context |
| `log.jsonl` | +4 | 2 dead_end, 2 decision |

Захваты-доказательства в `scratch/`: 7 `debug_*.log`, 13 `probe_log_*.jsonl`,
28 `screen_*.txt`, 4 `run_*_stream.jsonl`. Всего 760 КБ.

Прогонов Claude Code: 12 (5 headless `-p`, 7 интерактивных). Бюджет был «около 8»;
перерасход из-за того, что headless оказался непригоден для наблюдения канала и
все режимы пришлось переснимать интерактивно, плюс два прогона потеряны на
неработавший SendKeys.

## 2. Закрытие приёмки

- таблица 4 режимов × (баннер / `/mcp` / спавн / inbound / permission_request) —
  FINDINGS раздел 1;
- `--resume` и `--continue` **наблюдались** интерактивно, канал поднимается
  полностью, session id не меняется — раздел 3. Для headless записано отдельно,
  что канал не поднимается, с последствием для TASK-019 — раздел 2;
- user-scope без per-project consent — **подтверждено** в заведомо новой папке,
  ни диалога доверия, ни MCP consent — раздел 6;
- вложенный `claude -p`: второй экземпляр сервера поднимается молча и объявляет
  свой session id, но канальной регистрации не получает, то есть самостоятельной
  маршрутизируемой регистрацией не становится — раздел 7;
- точная команда MVP и alias для bash и PowerShell, «обёртки не будет» записано
  как принятое решение — раздел 9;
- поведение без флага (тихий drop, ни ack, ни ошибки) и что из этого следует для
  состояния «нет канала» в hub — раздел 8;
- probe и временная конфигурация удалены, секретов не осталось — раздел «Уборка»;
- тесты проходят — ниже.

## 3. Отклонения от спецификации

1. **Стенд 2.1.280, а не 2.1.278.** Claude Code обновился. Не блокирует, но все
   выводы относятся к 2.1.280; зафиксировано в шапке FINDINGS.
2. **Баннер и `/mcp` сняты не «глазами», а чтением экранного буфера консоли**
   через `ReadConsoleOutputCharacterW`. Спецификация допускала сказать «снять не
   удалось» — не понадобилось, оба экрана есть дословно.
3. **Permission relay для `Bash` увиден не в headless, а в интерактиве.** В `-p`
   permission_request не приходит вовсе (раздел 2), а команды, которые Claude
   Code считает read-only, вообще не спрашивают разрешения. Relay показан на
   записи в файл при manual mode.
4. **Вложенный прогон в N4 сам упал** на потере промпта через два слоя шелла.
   Вывод про регистрацию от этого не зависит: спавн probe и объявление session id
   произошли раньше ошибки, и тот же результат независимо виден в M1/M1b/M1c.

## 4. Тесты

```
cargo test --workspace
```

Всё зелёное: `parses_all_subcommands` ok, `subcommands_do_not_write_to_stdout` ok,
`transcript` lib и doc-tests — 0 тестов, 0 падений. Ни одного failed.

Отдельно `python scratch/selftest_probe.py` — 8/8 PASS (включая проверку, что
stdout probe остаётся чистым JSON-RPC).

## 5. Как проверить руками

Спайк одноразовый, probe уже снят с регистрации. Чтобы повторить:

```sh
claude mcp add --scope user probe -- python <repo>/maw/tasks/in_progress/TASK-004/scratch/probe_channel_server.py

# интерактивно, канал работает
CCTG_PROBE_LOG=<abs>/probe_log_manual.jsonl CCTG_PROBE_NONCE=MANUAL CCTG_PROBE_AT=10,40 \
  claude --dangerously-load-development-channels server:probe --debug-file <abs>/debug_manual.log
# в окне: отправить любой короткий промпт, дождаться блока "probe: PROBE-INBOUND-MANUAL-T10"

grep "Channel notifications" <abs>/debug_manual.log
grep -c PROBE-INBOUND ~/.claude/projects/<encoded-cwd>/<session-id>.jsonl

# без флага - тот же probe, сообщения выбрасываются
claude --debug-file <abs>/debug_noflag.log
grep "Channel notifications skipped" <abs>/debug_noflag.log

claude mcp remove --scope user probe
```

Скриптованный вариант: `python scratch/run_interactive_scenario.py <label> <sec>
--step "10:shot:banner" --step "16:type:<короткий промпт>\r" -- <аргументы claude>`.
Промпт держать коротким, иначе Enter его не отправит (раздел 11 FINDINGS).

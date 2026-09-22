# Verdict

NEEDS_WORK — ключевые выводы спайка подтверждаются, но обязательный сценарий настоящего интерактивного `SessionStart` не снят; вместо него использован top-level `claude -p`.

## Проверенный контрпример

До оценки проверялся конкретный опровергающий случай: наличие в `capture_*.jsonl` незаредактированного `C:\Users\user` (включая slash/MSYS/encoded-варианты), имени пользователя или реального значения переменной с `TOKEN`/`SECRET`/`KEY`/`SOCKET` в имени. Контрпример не подтвердился: совпадений домашнего пути и имени в captures нет, все чувствительные значения `CLAUDE_*` имеют вид `<redacted len=N>`, маркеров Telegram id/token нет.

## Confirmed correct

- Восьми реальным `SessionStart` соответствует `CLAUDE_CODE_SESSION_ID == stdin.session_id`; вложенные B/C/D действительно получают собственные `CLAUDE_CODE_SESSION_ID` и `CLAUDE_PID`. `CLAUDECODE=1` и `CLAUDE_CODE_CHILD_SESSION=1` также присутствуют у top-level, поэтому сами по себе вложенность не различают ([capture_A_toplevel.jsonl](scratch/capture_A_toplevel.jsonl:1), [capture_B_nested.jsonl](scratch/capture_B_nested.jsonl:1), [capture_D_nested_env_stripped.jsonl](scratch/capture_D_nested_env_stripped.jsonl:1)). Таким образом, `dead_end` из `log.jsonl` подтверждён первичными данными.
- Независимый повторный запуск [analyze_detect_parent.py](scratch/analyze_detect_parent.py:17) завершился с кодом 0 и совпал с сохранённым [detect_parent_report.txt](scratch/detect_parent_report.txt:1): ppid-путь правильно находит родителя в 3 из 4 заведомо вложенных запусков и не помечает снятые top-level запуски как nested.
- Сценарий с очищенным перед запуском окружением снят отдельно; дочерний Claude снова устанавливает собственные значения, а в цепочке виден parent Claude PID ([capture_D_nested_env_stripped.jsonl](scratch/capture_D_nested_env_stripped.jsonl:1)).
- Наборы полей `SubagentStart`/`SubagentStop` и оба пути получения отчёта подтверждаются captures: без `SubagentHandback` полный ответ находится в `last_assistant_message` ([capture_C_mixed.jsonl](scratch/capture_C_mixed.jsonl:8)); с `SubagentHandback` отчёт находится в `PreToolUse.tool_input.message` ([capture_unknown.jsonl](scratch/capture_unknown.jsonl:9)). Указанные `agent_transcript_path` и `.meta.json` существуют в `~/.claude/projects/**`; транскрипты подтверждают текст отчётов и `spawnDepth`.
- Временная project-scope настройка удалена: `.claude/settings.json` отсутствует, ссылок на `probe_hook.py` под `.claude/` нет. Production-файлы под `crates/`, workspace manifests и lockfile не изменены; набор зависимостей остаётся разрешённым.
- `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят без ошибок.
- Изменение нормативного знания корректно вынесено в `PCTX_PROPOSALS.md`, а не внесено напрямую в project context.

## Issues

### Major — не снят требуемый интерактивный старт

- **Файл:** [FINDINGS.md](scratch/FINDINGS.md:16)
- **Описание:** task.md требует два сценария, первый из которых — интерактивный старт. Все четыре top-level запуска в findings прямо обозначены как `claude -p`; `capture_A_toplevel.jsonl:1` имеет `CLAUDE_CODE_SESSION_ATTENDED=0` и `CLAUDE_CODE_ENTRYPOINT=sdk-cli`. `capture_unknown.jsonl` относится к уже запущенной интерактивной orchestrator-сессии и содержит только синтетический self-test `SessionStart`, а не настоящий интерактивный старт. Поэтому stdin/env/ppid именно интерактивного `SessionStart` не доказаны, а выводы частично обобщены с headless top-level на interactive без соответствующего capture.
- **Suggested fix:** снять отдельный redacted capture настоящего интерактивного `SessionStart`, прогнать его через анализатор и явно сопоставить четыре требуемые env-переменные и ppid-цепочку с headless top-level/nested результатами.

### Minor — полный набор `CLAUDE_*` не одинаков у top-level и nested

- **Файл:** [FINDINGS.md](scratch/FINDINGS.md:33)
- **Описание:** документ утверждает, что полный набор переменных одинаков, и включает `CLAUDE_CODE_EXECPATH`. Однако в top-level SessionStart сценария C этой переменной нет, а во вложенном SessionStart она есть ([capture_C_mixed.jsonl](scratch/capture_C_mixed.jsonl:1), [capture_C_mixed.jsonl](scratch/capture_C_mixed.jsonl:2)); та же разница есть в D. В A/B полный `claude_env_all` вообще ещё не записывался. Основной вывод о четырёх требуемых переменных от этого не страдает, но расширенное утверждение фактически неверно.
- **Suggested fix:** ограничить формулировку четырьмя проверяемыми переменными либо явно перечислить наблюдавшуюся вариативность `CLAUDE_CODE_EXECPATH`.

### Minor — причина обрыва цепочки C указана как факт без сохранённого доказательства

- **Файл:** [FINDINGS.md](scratch/FINDINGS.md:88)
- **Описание:** причиной промаха назван уже завершившийся parent bash над `env.exe`. Но capture C был сделан до добавления `ppid_chain_truncated_at`; для проблемной сессии отчёт содержит `n/a` ([detect_parent_report.txt](scratch/detect_parent_report.txt:26)). Из данных доказано только, что сохранённая цепочка заканчивается на `env.exe`, но не конкретная причина отсутствия следующего процесса.
- **Suggested fix:** назвать объяснение обоснованной гипотезой либо повторить этот сценарий с текущим probe, сохраняющим unresolved parent PID.

## Missing coverage

- Реальный интерактивный `SessionStart` с stdin, четырьмя требуемыми env-переменными и ppid-цепочкой.
- Синтетические replay-кейсы для ветвей контракта: отсутствующий `CLAUDE_PID`, найденный Claude-предок без записи в registry (`NestedUnknownParent`), ошибочный/stale PID и оборванная цепочка до родителя.
- Жизненный цикл registry: удаление записи на `SessionEnd` и защита от переиспользования PID. Findings справедливо помечает это как непроверенное ([FINDINGS.md](scratch/FINDINGS.md:208)).

## Nits

- Docstring [make_settings.py](scratch/make_settings.py:3) предлагает запустить отсутствующий `remove_settings.py`; лучше говорить только об удалении файла или приложить соответствующий helper.

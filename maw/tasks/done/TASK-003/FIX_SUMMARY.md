# FIX_SUMMARY — TASK-003 (fixer)

Спайк, production-кода нет. Изменены только файлы спайка в `scratch/`, `log.jsonl`
и `PCTX_PROPOSALS.md`. Под `crates/` ничего не трогалось.

## Проверенный контрпример (до правок)

Самая опасная претензия ревью — Minor #1: "FINDINGS утверждает, что полный набор
CLAUDE_* одинаков у top-level и nested, хотя CLAUDE_CODE_EXECPATH есть только у
вложенных". Если бы это было выдумкой ревьюера, правка испортила бы верный текст.
Проверено по сырым захватам: в `capture_C_mixed.jsonl` запись 1 (top-level) поля
`CLAUDE_CODE_EXECPATH` в `claude_env_all` нет, в записях 2 и 5 (вложенные) есть; то же
в `capture_D`. В `capture_A`/`capture_B` полного `claude_env_all` вообще нет.
Претензия настоящая, правка обоснована.

## Fixed

1. **Major — не снят настоящий интерактивный `SessionStart`.** Подтвердилось: первая
   запись `capture_unknown.jsonl` это синтетический self-test (`session_id
   test-sess-0001`), интерактивная orchestrator-сессия стартовала до появления хука.
   Снят новый захват: `scratch/launch_interactive.py` поднимает `claude.exe` в
   отдельной консоли (`CREATE_NEW_CONSOLE`) с окружением, очищенным от всех `CLAUDE_*`,
   ждёт `SessionStart` и убивает дерево процессов.
   Результат в `scratch/capture_E_interactive.jsonl`: `CLAUDECODE=1`,
   `CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDE_CODE_SESSION_ID == stdin.session_id`,
   `CLAUDE_PID == pid claude.exe`, `ATTENDED=1`, `ENTRYPOINT=cli`; в stdin дополнительно
   `scratchpad_dir` и `model`. То есть очистка окружения не помогает, env-правило не
   работает и для интерактива. Строки про интерактив добавлены в факт 1 и факт 2
   `FINDINGS.md`, файл добавлен в таблицу сценариев.

2. **Minor — `CLAUDE_CODE_EXECPATH`.** Формулировка факта 1 переписана: отдельно четыре
   переменные из задачи (видны всегда, во всех сценариях), отдельно остальной набор с
   явно описанной вариативностью `CLAUDE_CODE_EXECPATH` и оговоркой, что в A/B полный
   набор не писался.

3. **Minor — причина обрыва цепочки в сценарии C.** Переписано: зафиксированный факт —
   `ppid_chain_truncated_at=35316` в сценарии D (родитель `env.exe` отсутствовал в
   снапшоте); для A/B/C поле ещё не писалось, поэтому там тот же механизм назван
   обоснованным выводом по форме цепочки, а личность умершего процесса — гипотезой.
   Добавлено обратное подтверждение из сценария E: живой python-родитель — цепочка цела.

4. **Missing coverage — синтетические replay-кейсы веток контракта.** В
   `analyze_detect_parent.py` добавлен раздел `synthetic()`: отсутствующий `CLAUDE_PID`,
   незарегистрированный claude-предок, протухший/переиспользованный `CLAUDE_PID`,
   оборванная цепочка, срабатывание правила 1. Результаты в
   `detect_parent_report.txt` и таблицей в `FINDINGS.md`.
   Кейс с протухшим `CLAUDE_PID` вскрыл настоящий баг контракта: обход не пропускает
   собственный процесс и возвращает собственную же сессию как родителя. Добавлено
   замечание к реализации (проверять `parent != own session_id`).

5. **Nit — docstring `make_settings.py`.** Ссылка на несуществующий `remove_settings.py`
   убрана.

6. **Своя поправка, которой в ревью не было.** Прежний вывод "top-level во всех случаях
   определён как top-level" неверен. Все пять «top-level» запусков спайка запускались из
   Bash-тула сессии-имплементера через обёртку `env -u ...`
   (подтверждено `transcripts/6-implementer.jsonl`), то есть физически были вложенными,
   и вердикт `top_level` им дал обрыв цепочки над `env.exe`. Корректная формулировка:
   цепочка цела — родитель находится 4 раза из 4; цепочка оборвана — вложенность не
   видна 5 раз из 5. Настоящего ни от чего не порождённого top-level запуска в захватах
   нет, это вынесено в "что осталось непроверенным".

7. `PCTX_PROPOSALS.md` дополнен тремя уточнениями (интерактив, ширина дыры ppid,
   риск протухшего pid). Project context не редактировался.

## Skipped

- **Missing coverage: жизненный цикл registry (очистка на `SessionEnd`, защита от
  переиспользования pid).** Пропущено сознательно: это поведение будущего hub, кода
  которого ещё нет, спайк его проверить не может. В `FINDINGS.md` уже помечено как
  непроверенное; частично закрыто синтетическим кейсом с протухшим pid и новым
  замечанием к реализации. Задача этого не требовала.
- **Повторный прогон сценария C текущим probe** (альтернатива из ревью). Не делался:
  ревью допускало вариант "назвать гипотезой", а нужное доказательство механизма уже
  есть в сценарии D (`ppid_chain_truncated_at`). Лишний запуск потребовал бы снова
  положить `.claude/settings.json` и задеть живые сессии.

## Test results

    cargo test --workspace
    test result: ok. 1 passed; 0 failed   (cctg unit)
    test result: ok. 1 passed; 0 failed   (tests/stdout.rs: subcommands_do_not_write_to_stdout)
    test result: ok. 0 passed; 0 failed   (transcript lib)
    test result: ok. 0 passed; 0 failed   (doc-tests transcript)

    cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile — предупреждений нет

## Очистка и редакция (read-only проверка)

    ls -a .claude      -> . .. agents skills      (settings.json отсутствует)
    git status --short -> изменены только scratch/* и файлы задачи; .claude/ значится
                          как untracked каталог agents/skills, существовавший до задачи
    grep -rn "Users" scratch/*.jsonl scratch/*.txt scratch/*.md -> совпадений нет
    redact_encoded_cwd.py -> "clean" по всем семи захватам, включая новый E

Временный `.claude/settings.json` создавался на время захвата E и удалён сразу после;
запущенный `claude.exe` (pid 28764) убит `taskkill /T /F`, в списке процессов его нет.

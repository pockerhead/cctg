# TASK-004 — FIX_SUMMARY

Режим small-fix, спайк. `crates/`, `.claude/`, `CLAUDE.md`, `.env`,
`maw/project-context/` не тронуты. Коммитов нет.

## Preflight

- `scratch/` прочитан как карта покрытия; скрипты автора не перезапускались как
  «проверка».
- Самое опасное место ревью при дословном исполнении: «удалить оставшуюся запись,
  на которую срабатывает literal grep `probe`» в `~/.claude.json`. Grep по
  подстроке мог бы задеть чужую запись. Проверено кодом, печатались только имена
  ключей с `probe`: совпадение одно, это ключ `projects[<home>/AppData/Local/Temp/cctg_probe_dir]`,
  в `mcpServers` probe нет. Претензия реальна, но удалять надо ровно один ключ,
  а не «всё, где есть probe».
- Второе место: механическая замена путей в `*.jsonl` могла сломать JSON
  (экранированные `\\` и `\\\\` в debug-логах). `redact_home_paths.py` сверяет число
  невалидных строк до и после и отказывается писать при расхождении. Итог: 0
  невалидных строк.

## 1. Fixed

| пункт ревью | что сделано |
|---|---|
| Major: argv `--continue` / `--resume` не сохранён | Сырой stdout лаунчера (`cmd: ...`, `spawned pid N`) нашёлся в транскрипте implementer'а. `scratch/extract_transcript_evidence.py` вытаскивает его в `scratch/evidence_from_transcript.txt` раздел 1 и сверяет pid с `ppid` start-записи probe: N2 `--continue` pid 7732 = ppid 7732, N3 `--resume 684d8e75-...` pid 25092 = ppid 25092, I5 27708, N1 10724. N5 (`--continue --model haiku`, без флага): argv из tool_use, привязка через `CCTG_PROBE_SCENARIO=N5` и строку `not in --channels list` в `debug_N5.log`. Ссылки добавлены в FINDINGS разделы 1 и 3. Лаунчер `run_interactive_scenario.py` теперь до запуска пишет `run_<label>_argv.txt` (argv, cwd, nonce, start/end UTC, pid, exit status). |
| Major: `/mcp` снят только в fresh | Проверено: `/mcp` есть только в `screen_I4_*`. Повторные прогоны невозможны (см. Skipped). Клетки `/mcp` для `--resume`, `--continue` и без флага помечены **UNVERIFIED**. Фраза «ни с флагом, ни без него `/mcp` не пишет про channel» переписана: наблюдалось только с флагом. Цитата `/mcp` заменена почти дословным фрагментом (`probe · √ connected · 1 tool`, `Config location`). |
| Major: permission без флага не провоцировался | Подтверждено: в N5 было только «Say NOFLAG». Клетка стала «0 получено, но **UNVERIFIED**». В разделе 8 добавлено, что это ожидание, а не наблюдение. Повторный прогон невозможен (см. Skipped). |
| Major: вывод про nested слишком сильный | Раздел 7 переписан как найденный риск по формулировке оркестратора: второй агент со своим id и `ENTRYPOINT=sdk-cli`, inbound не получит, но `cctg agent` всё равно зарегистрируется в hub, значит hub должен распознать вложенность (контракт TASK-003) и приклеить к родителю или отклонить. Handshake с hub не проверялся. Сверх ревью исправлено следующее: implementer предлагал ловить вложенность по `ENTRYPOINT`, но `sdk-cli` будет и у headless resume, который hub запустит сам (TASK-019). Добавлено наблюдение про `CLAUDE_PID`: в env агента он наследуется, у вложенного это pid родительского claude (N4: 17440, ppid 2200). Раздел 10 поправлен так же. Записано в `log.jsonl` (decision) и в `PCTX_PROPOSALS.md` (дописана поправка). |
| Major: pre-launch «папки нет в конфиге» только в прозе | Сырой вывод `folder already known to claude: False` (17:46:50Z, за 6 с до N1) извлечён в `evidence_from_transcript.txt` раздел 2. Эта проверка строже точного ключа: ищется подстрока `cctg_probe_dir` в любом ключе. Дополнительно `check_trust_ancestors.py`: у папки нет доверенного предка (`~` есть с `hasTrustDialogAccepted=False`, `~/AppData/...` отсутствуют). Отсутствие trust-диалога этим не объясняется, записано как невыясненное. |
| Major: scratch не обезличен | `scratch/redact_home_paths.py`: все формы (`C:\…`, `C:/…`, JSON `\\`, двойной `\\\\`, `/c/…`) заменены на `~`, encoded `C--…` на `~enc`. Изменено 37 файлов. Проверка, только имена файлов: `grep -rilF` по семи вариантам (обратный слэш одинарный, двойной и четверной, прямой слэш, `/c/…`, `C--…`, `…-user`) дал 0 для каждого; `grep -rlE` по имени каталога профиля между любыми разделителями дал 0; регистрозависимый grep имени каталога профиля дал 0 файлов. Все строки `*.jsonl` парсятся. |
| Minor: fresh row приписывает Bash | Проверено: в `probe_log_I5.jsonl` только `mcp__probe__reply`. Клетка стала «да, MCP-tool 3 из 3 (I5); Bash в fresh не провоцировался», а Bash перенесён в строку `--continue`. |
| Nit: hidden flag без capture | Сырой результат `claude --help 2>&1 \| grep -i -A3 "channel"` (пустой вывод) в `evidence_from_transcript.txt` раздел 3, ссылка в разделе 9. |
| Nit: AppActivate без capture | Сырой вывод не пропал, он есть в транскрипте: `ACTIVATE_FAILED`, `send_keys rc: 1`, `new transcript files: []` (I2), теперь в `evidence_from_transcript.txt` раздел 4. Поэтому пункт не помечен как «несохранённое наблюдение», а снабжён ссылкой. |
| сверх ревью | Раздел 3: `input_preview` в `permission_request` приходит **строкой**, а не объектом (`probe_log_N2.jsonl`). Раньше FINDINGS показывал его объектом, hub на этом бы споткнулся. |

## 2. Skipped / blocked

- **Уборка `~/.claude.json` (пункт 1 оркестратора): заблокирована.** Запуск
  `cleanup_probe_project_key.py` auto-mode classifier отклонил как
  Self-Modification. В обход не шёл. Скрипт готов: удаляет один ключ, отказывается
  писать, если файл не проходит round-trip побайтно (проверено, проходит), и
  печатает только счётчики. Текущее состояние: вхождений `probe` в файле 1, это
  ключ `projects[<home>/AppData/Local/Temp/cctg_probe_dir]`. `mcpServers` чист.
  Временной папки нет. Нужен запуск пользователем:
  `python maw/tasks/in_progress/TASK-004/scratch/cleanup_probe_project_key.py AppData/Local/Temp/cctg_probe_dir`.
- **Повторные прогоны (пункт 3): не делались, 0 из 4 по бюджету.** Для каждого
  нужен `claude mcp add --scope user`, а это та же запись в `~/.claude.json`,
  которую только что запретили. Ещё одна причина: запуск в новой папке заводит
  новый ключ `projects`, а убрать его я не могу. `--continue` в папке репо взял бы
  живую сессию оркестратора. Поэтому `/mcp` в трёх режимах и no-flag
  permission остались **UNVERIFIED** в таблице. Вопрос с argv закрыт без прогонов
  (см. Fixed).
- **Hub integration для nested:** не требовался, hub ещё нет.
- **История git:** в коммите `b2dbc94` захваты лежат ещё с домашним путём.
  Редакция касается только рабочего дерева. Переписывать историю не моё решение,
  это остаётся оркестратору и пользователю.
- `debug_*.log` в gitignore (`*.log`), в коммит они не попадут. Редакция к ним
  всё равно применена.

## 3. Test results

```
cargo test --workspace
test tests::parses_all_subcommands ... ok
test result: ok. 1 passed; 0 failed; ...
test subcommands_do_not_write_to_stdout ... ok
test result: ok. 1 passed; 0 failed; ...
test result: ok. 0 passed; 0 failed; ...   (transcript lib)
test result: ok. 0 passed; 0 failed; ...   (doc-tests)
```

`python -m py_compile` для изменённых и новых скриптов: ок. Процессов claude
или python я не запускал, прибирать нечего.

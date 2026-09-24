# TASK-018 QA_REPORT (qa, claude opus, medium)

Verdict: **SHIP**

## 0. Disconfirmation (сделано первым)

Самый конкретный контрпример: настоящий `cctg hook SessionStart` при выключенном hub, потом hub поднимается, потом следующий настоящий хук этой сессии. Реализация неверна, если выполняется хотя бы одно: сессия приходит в hub дважды или не в том порядке (prompt раньше start); появляется вторая тема; повторно подложенный доставленный файл снова доходит до актора; выключенный hub стоит хуку больше одного бюджета при нескольких файлах в спуле.

Проверил своим тестом (`scratch/qa/qa018_spool_slot.rs.txt`, прогон `qa018_spool_slot.out.txt` и `.run2.txt`). Контрпример не подтвердился: в актор пришли `session_start`, `user_prompt_submit`, `stop`, по одному разу и в этом порядке. `createForumTopic` был один. Подложенная копия start отброшена дедупликацией hub. Бюджеты при выключенном hub укладываются в один.

Отдельно проверил код. Спул может заблокироваться, если hub навсегда отказывает сохранённому файлу с 4xx: replay останавливается на первой ошибке, и события сессии не уходят до 24 ч. На практике 400 здесь даёт только несовпадение формата hub и хука. Wire VERSION по правилам проекта не меняется, а в таком случае и собственные события хука тоже не проходят. Дефектом это не считаю, записал в хвосты.

## 1. Environment

- Без docker. Прямые прогоны cargo: один `CARGO_TARGET_DIR=$TEMP/cctg-018-qa-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, `--offline`, cargo по одному. Каталог удалён.
- Настоящие бинарники `cctg hook` и `cctg agent`, настоящие `serve_hooks`, `serve_agents`, `Slots`, `Scheduler` и `updates::poll`. Telegram фейковый. Home, state и `CLAUDE_CONFIG_DIR` лежат во временных каталогах. Настоящие Telegram, `.env`, `device.env` и `~/.claude.json` не использовались. Живой режим не запускался.
- Воспроизведение (Git Bash, корень репо):
  ```
  export CARGO_TARGET_DIR="$TEMP/cctg-018-qa-target" CARGO_PROFILE_DEV_DEBUG=0
  cargo fmt --all -- --check
  cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings
  cargo test -j 1 --offline --workspace --no-fail-fast
  cargo test -j 1 --offline -p cctg --test soak -- --ignored     # x3
  python maw/tasks/in_progress/TASK-018/scratch/qa/mutate.py Q1|Q2|Q3
  cp .../scratch/qa/qa018_spool_slot.rs.txt crates/cctg/tests/qa018_spool_slot.rs
  cargo test -j 1 --offline -p cctg --test qa018_spool_slot -- --nocapture
  rm crates/cctg/tests/qa018_spool_slot.rs; rm -rf "$TEMP/cctg-018-qa-target"
  ```

## 2. Test results

- `fmt --check`: exit 0 (`scratch/qa/fmt.txt`).
- `clippy --workspace --all-targets -D warnings`: exit 0 (`clippy.txt`).
- `cargo test --workspace`: exit 0 (`workspace_test.txt`). cctg lib 407 passed, 1 ignored. hook_cli 8, spool_e2e 4, stream_e2e 11. Остальные цели зелёные, `soak: skipped`.
- Фейковый soak, три прогона подряд (`soak_fake.txt`): три раза exit 0 и `soak: ok`, 15.3-15.5 с, 102/102/104 вызова. Каждый раз 3 темы и 1 разделитель. Service messages 13/13/14, все удалены. Оба 429 были, после каждого пауза и один повтор. Задержка permission: A 15/18/138 мс, A #2 61/61/294 мс, без пометки о паузе 429. 28 строк A #2 ушли после prompt. После каждого прогона каталогов `cctg-soak-*` 0.
- Мутации против текущего кода (`mutations.out.txt`, `mut_Q*.log`; файлы восстановлены, `git status` по `crates` чистый):
  - Q1: у permission нет приоритета (`next_permission` -> None). **KILLED**, но на проверке "both planned 429s fell into the burst" (строка 1401), то есть побочно: prompt уходит после всплеска, и 429 не срабатывают.
  - Q3: Q1 плюс отключённая проверка из строки 1401. **KILLED** на `the prompt of A2 overtook stream lines of its own topic written before it` (строка 1414). Утверждение о приоритете не пустое.
  - Q2: 429 не ставит очередь на паузу. **KILLED**: `a call 155ms after a 429` (строка 1589).
- Свой e2e спула (`qa018_spool_slot`, 2 прогона, exit 0). Тест запускает настоящий `cctg hook`, `CLAUDE_PID` убран:
  1. Hub выключен, `SessionStart`: stderr "kept for the next hook", в спуле 1 файл. Время 541 мс на тёплом старте, 982 мс на холодном первом exe. Сам POST ждал 499.9 мс, то есть один бюджет, остальное стоит запуск процесса.
  2. Hub выключен, `UserPromptSubmit` при сохранённом start: 316-318 мс, один бюджет 300 мс вместо 300+500. Prompt не сохранён.
  3. Hub выключен, 15 файлов в спуле, `SessionEnd`: 528-532 мс, меньше 1.5 с. End сохранён 16-м.
  4. Hub поднят, следующий `UserPromptSubmit`: stderr пуст. В актор пришли start и затем prompt, спул и каталог сессии удалены.
  5. Доставленный start подложен обратно, затем `Stop`: в актор дошёл только `stop`, start повторно не пришёл. `createForumTopic` ровно один. В `registry.json` один слот с `current_session` = S1 и `kind: top_level`.
  6. stdout хука пуст, в stderr нет id сессии и секрета.
- Уборка: `%TEMP%` `cctg-test-*`, `cctg-soak-*`, `cctg-qa018-*`, `cctg-spool-e2e-*` пусто (0). `~/.cctg/spool` не появился. Target удалён.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| пять запусков, ровно три темы, ни одного перепутанного маршрута | soak x3: `create_topic == 3` до и после фазы 5; проверки "shows in the wrong topic" и inbound по id | PASS |
| пятая сессия при выключенном hub занимает освободившийся слот, один разделитель, без новой темы | soak фаза 5 (`creates_before == 3`, `slots[0].current_session == A5`, один `separator`); мой e2e: одна тема, порядок start -> prompt | PASS |
| всплеск по политике планировщика, 429 по `retry_after`, без retry storm | soak: бакет и min_gap проверены по окнам 1/3/60 с, пик 92%; 429: пауза не меньше retry_after и ровно один повтор; мутация Q2 убита | PASS (fake) |
| permission впереди очереди транскрипта, задержка измерена и записана | soak: `behind_a2 > 0`, задержки в отчёте без паузы 429; мутация Q3 убита самим этим утверждением | PASS |
| `registry.json` точно описывает 3 слота, текущие сессии и связь nested с родителем | soak: точные наборы ключей и значения (slots, sessions, nested block, pids) | PASS |
| отчёт отдельно считает send / edit / topic-create и не сравнивает правки с 20/мин | отчёт soak: таблица по методам и явная строка "not compared with the 20 messages/min" | PASS |
| нет накопившихся `forum_topic_edited` | soak: shown == edit_topic, left = 0 (13/14 удалены) | PASS (fake) |
| недоставленный `SessionStart` досылается следующим хуком; спул ограничен, без секретов и текста, повтор без дублей | spool_e2e 4 теста, spool unit-тесты, мой e2e (настоящий хук + serve_hooks + Slots): один раз, по порядку, дедупликация, Stop и prompt не сохраняются, stderr без id | PASS |
| Existing tests pass | fmt, clippy -D warnings, workspace test | PASS |

Безопасность живого режима, проверена только чтением кода:
- Трогаются только свои темы. Входящие и callback из настоящего чата в live не маршрутизируются (`Routed::Input/Callback if !live`). Hub удаляет service messages только через `slot_by_topic`. Отправок в General (`thread_id: None`) в slots, buffer, permissions, stream и subagents нет. `delete_topics` берёт только свои успешные `create_topic`.
- Синтетические вызовы: `React` на id от 2e9 и `AnswerCallback` с префиксом `soak-` транспорт отвечает сам и в Telegram не отправляет.
- Уборка. При успехе и при панике сценария: он идёт в отдельной задаче, затем удаление тем, затем `resume_unwind`. Есть `Drop` у `Hub` и `Sim`. При сбое подготовки срабатывает `RemoveOnDrop(root)`, тем в этот момент ещё нет.
- Секреты не печатаются. `ConfigError` без значений, `ApiError::Http` через `without_url()`. Ошибки `delete_topics` отбрасываются непрочитанными. В stderr только число ждавших апдейтов. Subscriber `tracing` в soak нет.

## 4. Bugs found

Дефектов, ломающих поведение, не нашёл. Замечания:

1. **low (сила теста)**, `crates/cctg/tests/soak.rs:1401`. После правки фиксера мутацию "permission без приоритета" первой ловит проверка про 429, а не проверка приоритета. Сама проверка приоритета рабочая (Q3). Но если проверку 429 когда-нибудь ослабят, об этом никто не узнает. Исправлять не обязательно.
2. **low (известное ограничение, не описано)**. Если hub навсегда отказывает сохранённому файлу с 4xx (400 или 413), replay каждый раз останавливается на нём. Все следующие события этой сессии, включая ответы `Stop`, не уходят, пока файл не станет старше 24 ч. На практике это только рассинхрон формата hub и хука.
3. **info**. Задержки permission сильно скачут между прогонами (A #2 от 45 до 294 мс, у ревьюера кода до 1053 мс до правки). Абсолютное число говорит о загрузке машины, а не о политике.
4. **info (документированные ограничения)**. `SessionEnd` сессии, закончившейся при выключенном hub, никто не досылает, поэтому новая сессия в той же папке получит `#2`, а не освободившийся слот. Сценарий soak этот случай обходит: A1 заканчивается при работающем hub. Ещё есть окно около 500 мс, когда агент делает replay раньше, чем хук положил start в спул.

## 5. Verdict

**SHIP.** Все пункты приёмки подтверждены моими прогонами: workspace зелёный, fmt и clippy чистые, soak 3/3, три мутации убиты. Две из них показывают, что утверждения о приоритете permission и паузе 429 не пустые. Отдельный e2e спула на настоящих процессах прошёл. Живой прогон в Telegram (настоящие 429 и `forum_topic_edited`) не выполнялся по заданию, эти пункты подтверждены только фейком.

# TASK-047 — fixer summary

Режим small-fix, отдельного ревью-файла нет: действовал по дополнениям оркестратора (пункты 1-4 в промпте) и по результату пробы `scratch/probe/probe_resume_agent.out.txt`. Код: коммит `a06b3eb` на `fix/restart-guard` поверх `a71db4c`.

Preflight, самое конкретное утверждение, которое сломало бы код при дословном исполнении: пункт 2 «сессия с субагентом, у которого был SubagentStart и не было SubagentStop, считается "агенты работают"». Проверка в `crates/cctg/src/hook.rs` (`"SubagentStop"`): хук выкидывает стопы без файлов субагента как «internal agent», а typed start агента `--agent` отличить нельзя (комментарий у `"SubagentStart"`). Дословная реализация держала бы обновление до 6 ч после каждого внутреннего агента в такой сессии. Поэтому в счёт идёт только агент, который ещё кандидат или уже сопоставлен с вызовом `Agent` (механизм TASK-015); внутренний выпадает по окну кандидата (60 с).

## 1. Fixed

| Пункт | Что сделано |
|---|---|
| 1. Диалог выхода | `keys::exit_dialog(screen)`: панель под последней кромкой `▔` без поля ввода после неё, в ней строка `Background work is running`. `type_exit` после Enter до 2 с смотрит на экран; нашёл диалог, жмёт Esc (до 3 раз, только пока диалог виден), дальше `Typed::Agents`, если диалог закрылся, и `Typed::Failed`, если нет. Вариант 1 или 2 не выбирается никогда. `Worker::restart` удаляет файл заявки при любом исходе кроме `Sent`, агент отвечает `agents_running` (путь уже был в `agent.rs`). Фикстура: экран диалога из пробы; отрицательные случаи: тот же текст в разговоре над полем ввода, панель `/cost`, экран после Esc. |
| 2. Сигнал hub | `Slots.started_agents` (agent id → сессия, срок), заполняется в `track_agents` на typed `SubagentStart` живой top-level сессии, снимается на `SubagentStop`, при конце сессии (тот же `retain`, что у `activity`: SessionEnd, `/clear`, reaper) и по сроку `AGENT_MAX_AGE` = 6 ч. `agents_running(session)`: запись не старше срока и агент ещё кандидат (`Candidates::contains`) или есть в `registry.subagents` этой сессии. `pump_updates`: пока список не пуст (и update не в полёте), `update` не шлётся, press не истекает, в тему один раз `UPDATE_AGENTS_NOTICE` (общий флаг `agents_told` с ответом агента). Продолжение сразу на следующем `pump` после последнего стопа; срок годности записи попадает в `next_deadline`. |
| 3. Текст продолжения | `continue_text(agents)`: без агентов только `CONTINUE_TEXT`; с агентами добавляется «Перезапуск остановил фоновых субагентов с agentId: <ids>. Возобнови каждого через SendMessage, указав в to его agentId (не имя: по имени агент недоступен); он продолжит по своему транскрипту с места остановки.» Маркер PROBE-DEPENDENT убран. Id берутся из `agents_running` в момент ответа `restarting` и лежат в `SessionEntry.restart_agents` (serde default, пустой не пишется), флаг ставится и тогда, когда работа не прервана, но агенты есть. `keep_agent` и отправка очищают оба поля. |
| 4. Ревью a71db4c | Баг детектора: в пробе строка работающего фонового агента на обычном экране `◯ general-purpose  Append steps with sleeps  4s · ↓ 39.6k tokens`, `agent_row` знал только `( )`, `●`, `(x)`. Список сводился к `main`, и экран не блокировал набор. Добавлен `◯` (U+25EF) как невыбранный радио-маркер; тест `a_working_agent_on_the_main_screen_blocks_typing` на двух экранах пробы (после запуска и после resume через SendMessage). |

Попутно: `UPDATE_AGENTS_NOTICE` теперь «⏳ Обновление клиента ждёт, пока закончат фоновые агенты, и продолжится само.» Удержание со стороны hub срабатывает и для замены воркера без перезапуска, поэтому слово «Перезапуск» не подходило. `panel()` и `exit_dialog()` делят помощник `panel_edge`.

Новые тесты: `keys::tests::{a_working_agent_on_the_main_screen_blocks_typing, the_exit_dialog_is_seen_only_as_a_panel}`, `hub::slots::tests::{a_subagent_seen_running_holds_the_update_until_it_stops, a_lost_subagent_stop_holds_the_update_only_so_long, a_restart_that_stops_subagents_names_them_in_its_message, the_continuation_names_stopped_subagents_only_when_there_are_some}`. Тест `a_restart_that_cut_off_work_tells_the_next_agent_once` теперь ждёт `[CONTINUE_TEXT]`.

## 2. Skipped / не решено

- Hub не удерживает релиз на ответе `restarting`, если субагент стартовал уже после отправки `update`. Это узкая гонка, и её закрывает диалог выхода: Esc, `agents_running`, агент привязан обратно, флаги сняты. Если держать и здесь, список агентов для продолжения никогда бы не заполнялся. Решение записано в log.
- `started_agents` живёт только в памяти. После рестарта hub сигнала нет, пока не придёт новый SubagentStart; остаются экран и диалог.
- В сессии без агента, читающего транскрипт (старый клиент без `session_reads`), кандидат не сопоставляется и через 60 с перестаёт считаться. На такой случай остаются экран и диалог.
- Не проверено вживую: как выглядит строка уже закончившегося фонового агента в списке. Если там остаётся время (`2m 3s`), экранный детектор держит перезапуск, пока список виден, а press при ответах `agents_running` не истекает. Проверить можно только интерактивной пробой, а она мне запрещена. Кандидат на следующую пробу.
- Прерванный ⏹ foreground-субагент без `SubagentStop` считается не дольше окна кандидата (60 с после старта или стопа). Стреляет ли SubagentStop на прерывание, неизвестно.

## 3. Test results

`CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`:

- `cargo fmt --all -- --check`: чисто.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: `Finished`, без предупреждений.
- `cargo test -j 1 --workspace`: exit 0; lib `cctg`: `test result: ok. 611 passed; 0 failed; 1 ignored` (было 605 + 6 новых); все 41 строки `test result` ok, `run_e2e: ok`, `supervise_e2e: ok`, `soak` пропущен как медленный (как и раньше).

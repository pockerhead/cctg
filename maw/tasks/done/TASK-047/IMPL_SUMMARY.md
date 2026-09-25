# TASK-047 — implementer summary

Режим small-fix, отдельного плана нет. Pre-flight: всё, что названо в заметках оркестратора, есть и имеет ожидаемую форму (`keys::{type_line, type_command, input_box, panel}`, `update::Worker::{plan, restart}`, обработка `update`/`released`/`console_command` в `agent::serve_channel` и `spawn_console`, `Slots::{press_update, pump_updates, on_update_answer, busy}`, `UpdateAsk`, `Activity`, уведомления в `hub/status.rs` и `hub/console.rs`). У `UpdateOutcome` и `CommandOutcome` есть `#[serde(other)] Other`, так что новые варианты аддитивны.

Код: коммит `a71db4c` на `fix/restart-guard`.

## 1. Что сделано

| Файл | +/- | Что |
|---|---|---|
| `crates/cctg/src/keys.rs` | +223 / -2 | `agents_block(screen)`: детектор; `Typed::Agents`; `type_and_watch` читает экран до набора и ничего не набирает при блоке; `agents_on_screen(pid)`: прочитать экран без набора; тесты на фикстурах живого случая |
| `crates/cctg/src/wire.rs` | +36 | `UpdateOutcome::AgentsRunning`, `CommandOutcome::AgentsRunning` (`"agents_running"`), round-trip и тест имени |
| `crates/cctg/src/update.rs` | +10 | `Worker::agents_on_screen()`, doc модуля |
| `crates/cctg/src/agent.rs` | +14 | `Plan::Restart`: если экран блокирует, ответ `agents_running` без ухода от hub; после `released` `Typed::Agents` -> `agents_running`; команда -> `CommandOutcome::AgentsRunning` |
| `crates/cctg/src/hub/slots.rs` | +334 / -10 | удержание перезапуска, повтор, уведомление, флаг и сообщение-продолжение, отказ команды; 4 теста |
| `crates/cctg/src/hub/status.rs` | +10 / -1 | `UPDATE_AGENTS_NOTICE`, `Activity::working()` |
| `crates/cctg/src/hub/console.rs` | +7 / -2 | `AGENTS_NOTICE` |
| `crates/cctg/src/hub/registry.rs` | +5 | `SessionEntry.restart_interrupted` (serde default, не пишется при false) |

### Детектор (`keys::agents_block`)
Блокирует, если на экране:
- строка поля ввода вида `<❯|>` + пробел/NBSP + `Message @<непробельный символ>` (плейсхолдер вида субагента). `input_box` для этого не нужен: в живом случае поле не нашлось между двумя линейками;
- список агентов: ищется строка, где `main` стоит один (`( ) main` или `● main`), берётся непрерывный блок строк-агентов вокруг неё. Блок, если у любой строки не-`main` маркер (`●` или заполненный радио `(x)`) или слово-таймер (`35m`, `15s`, `1h`, `2m30s`).

Без строки `main` список не распознаётся: `●` начинает и строки вызовов инструментов в транскрипте. Всё остальное не блокирует (как раньше). Обычный экран с `← 1 agent` в статусе проходит.

### Перезапуск
- Агент проверяет экран дважды: перед ответом `restarting` (тогда отвечает `agents_running` и остаётся привязанным, без `released` и без обрыва чтений) и ещё раз прямо перед `/exit` (гонка: тогда `agents_running` приходит после `restarting`, hub привязывает агента обратно).
- Hub на `agents_running` (для `sent` или `left` этого update): press остаётся, раунд не считается, `until` сдвигается на `UPDATE_WAIT`, `retry_at = now + UPDATE_RETRY` (30 с; `next_deadline` будит actor), в тему один раз «Перезапуск ждёт, пока закончат фоновые агенты…» (`agents_told`, плюс обычный `notify`-лимит). `pump_updates` не шлёт `update` до `retry_at`.
- Старый hub читает `agents_running` как `Other` → «Обновить клиент не получилось», press снят, ушедший агент привязан обратно. Безопасно.

### Команды из темы (TASK-043)
`Typed::Agents` → `CommandOutcome::AgentsRunning` → ответом на сообщение `console::AGENTS_NOTICE`.

### Сообщение-продолжение
- `UpdateAsk.interrupted` ставится, когда ⏹ записал Esc в консоль сессии, пока press ждал.
- На принятом `restarting` (агент отпущен) hub ставит `registry.sessions[s].restart_interrupted = true`, если `ask.interrupted` или `Activity::working()` (ход или вызов ещё идёт, в том числе остановленный Esc, но конец ещё не виден).
- Флаг снимается, если ушедший агент привязан обратно (`keep_agent`: draft, failed, agents_running, таймаут) — перезапуска не было.
- `send_continuations()` в начале `pump` (до `flush_all`): для сессии с флагом, у которой есть живой привязанный агент в её слоте, одно `HubMsg::Inbound` с meta `chat_id`, `thread_id` и текстом `CONTINUE_TEXT + " " + CONTINUE_AGENTS_TEXT`; флаг снимается, когда очередь линка взяла сообщение.
- `CONTINUE_AGENTS_TEXT` в `hub/slots.rs` помечен `PROBE-DEPENDENT (TASK-047)`, сейчас «Если работали фоновые агенты, проверь их и при необходимости запусти заново.» Оркестратор меняет его по результату пробы.
- Перезапуск в простое и замена воркера (`reloading`) флаг не ставят.

## 2. Отклонения и чего нет
- Сверх задачи: экран проверяется ещё и до ответа `restarting`. С проверкой только перед `/exit` каждый повтор раз в 30 с отпускал бы агента и обрывал его чтения (`fail_reads_of`) на всё время работы фонового агента. Проверка перед `/exit` осталась для гонки.
- Press, ждущий агентов, не истекает, пока агент отвечает `agents_running` (`until` сдвигается на каждом ответе, как при идущем ходе). Если агент пропал, press забывается через 120 с.
- Сообщение-продолжение всегда содержит фразу про фоновых агентов (её формулировка условная). Hub не проверяет, работали ли агенты на самом деле.
- Известное ограничение: на обычном экране (список агентов закрыт) работающий фоновый агент может быть не виден. Как выглядит строка статуса с работающим агентом, неизвестно, а незнакомые формы по условию не блокируют. Второй возможный сигнал есть в реестре hub: блоки субагентов без `SubagentStop`. Сюда не добавлял, потому что задача описывает детектор по экрану. Решать оркестратору или по пробе.
- Флаг `restart_interrupted` висит, пока сессия не вернётся. Если `cctg run` не поднял claude и сессию подняли вручную через дни, сообщение придёт тогда.

## 3. Тесты
- `cargo fmt --all -- --check`: чисто (после `cargo fmt --all`).
- `cargo clippy --workspace --all-targets -j 1 -- -D warnings`: чисто.
- `cargo test --workspace -j 1` (`CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`): всё зелёное, lib 605 passed / 1 ignored, остальные бинарники 0 failed.
- Новые тесты:
  - `keys::tests::the_agent_view_and_working_background_agents_block_typing`: живой экран (плейсхолдер + список), каждый признак по отдельности, `(•)`/`●`, таймер у невыбранной строки.
  - `keys::tests::a_plain_screen_does_not_block_typing`: `← 1 agent`, строки инструментов `● Bash(...)` с `(5s)`, `● main is up to date 5s ago`, `Message me…`, строки без `main`, слова-таймеры.
  - `wire::tests::background_agent_outcomes_have_their_own_names` + round-trip образцы.
  - `hub::slots::tests::a_restart_waits_for_background_agents_tells_once_and_is_asked_again`: больше ответов, чем `UPDATE_ROUNDS`, агент не отпущен, повтор только после `retry_at`, путь «после released», одно уведомление, затем перезапуск проходит.
  - `hub::slots::tests::a_command_refused_for_background_agents_is_answered`.
  - `hub::slots::tests::a_restart_that_cut_off_work_tells_the_next_agent_once`: ⏹ во время press, конец хода виден до перезапуска, флаг в `registry.json`, SessionEnd → новый агент → SessionStart(resume) → ровно одно `Inbound`, повторный `pump` ничего не шлёт.
  - `hub::slots::tests::an_idle_restart_and_a_refused_one_tell_the_session_nothing`: отказ (draft) после ⏹ снимает флаг; перезапуск в простое ничего не шлёт.

## 4. Как проверить руками
1. Сессия под `claude-cctg` с устаревшим клиентом. Запустить фонового субагента с долгой задачей, открыть его вид (← в списке агентов). Нажать «Обновить» в теме: в тему один раз приходит «Перезапуск ждёт, пока закончат фоновые агенты…», `/exit` не набирается, в debug-логе агента `update answered outcome=AgentsRunning`. Закрыть вид / дождаться конца агента: в течение ~30 с перезапуск проходит сам.
2. В той же ситуации отправить в тему `/cost`: ответ «В терминале открыт вид субагента или работают фоновые агенты…».
3. Во время хода нажать «Обновить», потом ⏹ (подтвердить): после перезапуска в сессию приходит один `<channel source="cctg" ...>` с текстом «Клиент cctg обновлён, и сессия была перезапущена посреди работы…».
4. «Обновить» в простой сессии: после перезапуска сообщения нет.

Scratch: `scratch/implementer/edit_slots.py`, `edit_slots2.py` (разовые правки `slots.rs`). Предложения в контекст проекта: `PCTX_PROPOSALS.md`.

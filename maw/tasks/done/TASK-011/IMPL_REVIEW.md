# IMPL REVIEW — TASK-011 (hub: слотовый реестр и жизненный цикл тем)

Stage: code-reviewer (claude/opus, effort=medium). Ревью по коду `git diff main -- Cargo.toml Cargo.lock crates/` на HEAD `9457d7e`, а не по IMPL_SUMMARY.

## 1. Verdict

**NEEDS_WORK**. Все 10 критериев приёмки закрыты кодом и тестами, но правило `/clear` из плана (новая сессия того же процесса остаётся в своём слоте) работает только в одном порядке хуков. В порядке SessionEnd, потом SessionStart тема перескакивает в первый свободный ordinal. Второй дефект: известная top-level сессия после одного вложенного старта навсегда становится вложенной. Оба воспроизведены пробными тестами.

## Disconfirmation

Контрпример, записанный до оценки: "`/clear` в сессии B (слот `#2`), когда `#1` свободен, и хуки приходят в естественном порядке: сначала `SessionEnd(B)`, потом `SessionStart(C, source=clear, тот же claude_pid)`. Если реализация верна, C остаётся в `#2`."

Проверка по коду: `apply_hook(SessionEnd)` удаляет `pids["host/pid"]` (`registry.rs:616-621`). В `allocate` правило `/clear` ищет прежнюю сессию только через `self.pids` (`registry.rs:456-470`), поэтому ничего не находит и уходит в "первый свободный ordinal" (`registry.rs:471-481`). Пробный тест `crev_clear_in_real_hook_order_jumps_topic` в копии HEAD: **FAILED, "clear jumped from #2 to #1"**. Контрпример подтвердился. Тест реализации `clear_in_one_process_stays_in_its_slot` (`registry.rs:1353`) проверяет только обратный порядок (SessionStart раньше SessionEnd). Доказательства: `T/scratch/crev/probe_registry_tests.rs`, `probe_out.txt`, `probe_README.txt`.

## Проверка

- LF sha256 всех 6 файлов совпали с таблицей PLAN_FINAL (`verify_hashes.sh`: 6 × OK). `Cargo.toml`/`Cargo.lock` не менялись, новых крейтов нет.
- `cargo fmt --all -- --check`: чисто. `cargo clippy --workspace --all-targets --offline -- -D warnings`: чисто.
- `cargo test --workspace --offline`: 147 + 1 + 1 + 1 + 1 + 1 + 3 + 10 + 15 + 3 + 19 + 14 + 14 + 1 = **231 passed, 0 failed, 1 ignored**, совпадает с планом.
- `closeForumTopic`: в `crates/` нет ни одного вхождения (`close_forum`, `closeForumTopic`, `CloseTopic`), в `scheduler::Op` такой операции тоже нет.
- Лог-записи dead_end (planner: порядок в `select!`; reviewer-2: repro двойной замены) проверены по коду. Тесты ждут op хука перед агентом, `/clear`-repro живёт в `slots::a_gone_topic_during_a_session_change_is_replaced_once`.

## 2. Confirmed correct

- **Критерий 1** (сессия после смерти: 0 тем, 1 разделитель): `allocate` берёт первый свободный слот папки (`registry.rs:471-481`), `occupy` ставит ровно один `pending_separator` (`registry.rs:499-509`). Тесты `a_session_after_a_dead_one_reuses_the_slot_with_one_separator`, `slots::one_slot_lives_through_hook_agent_end_and_the_next_session`.
- **Критерий 2** (`#2`, `#3`): `ordinal = max + 1`, `#N` только при `ordinal > 1` (`registry.rs:213-217`). Освободившийся `#2` занимается раньше, чем появится `#4`.
- **Критерий 3** (nested/subagent): `nesting()` и ранний `Parent(None)` для pid самой сессии (`registry.rs:532-540`). Субагенты с пустым `agent_type` отбрасываются (`registry.rs:654`). Вложенные сессии слот не занимают.
- **Критерий 4**: SessionEnd меняет только `ended`, и `topic_work` даёт `EditTopic{name: None, icon: DEAD}`. Иконки берутся только из успешного `getForumTopicIconStickers` (`mod.rs::checked_icons`, `Icons::from_offered`). Меньше 4 id или ошибка запроса дают ошибку старта.
- **Критерий 5**: `topic_title` считает UTF-16 через `telegram_len`, host режется до 32 единиц, первым режется label, за ним folder. `[host]` и `#N` сохраняются. Арифметика `folder_room` не уходит в минус: head ≤ 34, suffix ≤ 12 при `u32::MAX`. Тест проходит кириллицу, эмодзи и `u32::MAX`.
- **Критерий 6**: `save` пишет temp, делает `sync_all` и `rename` (`registry.rs:987-994`). `load` без содержимого файла в ошибке (`LoadError::Invalid` без serde-текста). `topic_invalid` срабатывает только для текущего `thread_id`, а `busy` держит один вызов на слот, поэтому поздние отчёты по старой теме ничего не меняют.
- **Критерий 7**: `on_control` удаляет `forum_topic_edited` только в теме известного слота и только при `can_delete`. Первая ошибка удаления даёт один `warn`, дальше идёт `debug` (`slots.rs:338-345`). Изолированный тест `tests/slots_logs.rs` проверяет то же и отсутствие приватных строк.
- **Критерий 8**: хук-сессия получает `NoChannel` (👀). Агент до хука ждёт в `pending` и привязывается первым хуком, при этом вторая тема не создаётся (`slots.rs:275-286`).
- **Критерий 9**: `folder_key` снимает `\\?\` и `\\?\UNC\`, меняет `\` на `/`, убирает хвостовые `/`, делает case-fold только для drive/UNC. `folder_name` сохраняет исходное написание. `cwd.get(..8)` не паникует на не-char-boundary.
- **Урок TASK-010 QA**: в `Slots::run` нет `await` кроме `select!`. `hand_off` уходит в unbounded dispatch, а ждёт `dispatch_loop`. Тест на 1100 SessionStart при застрявшем транспорте проходит.
- Логи содержат только short id, ordinal и фиксированный текст. `LoadError` и ошибка сохранения логируют `ErrorKind`, не путь.
- `SlotLocator`: тема слота без префикса даёт текущую сессию слота, всё прочее уходит в `ProjectsDir`. Пустой путь даёт `NoTranscript` с русским текстом.

## 3. Issues

### I1 — major — `/clear` перескакивает в другую тему в естественном порядке хуков
`crates/cctg/src/hub/registry.rs:456-470` (вместе с `616-621`).
Правило "слот предыдущей сессии того же `host/claude_pid`" ищет прежнюю сессию только через `pids`. SessionEnd эту запись удаляет. При `/clear` Claude Code шлёт `SessionEnd(reason=clear)` и `SessionStart(source=clear)` из двух отдельных hook-процессов. Если SessionEnd доходит первым, а по документации он и стреляет первым, новая сессия попадает в первый свободный ordinal, а не в свой слот. Даже если порядок на хабе недетерминирован, правило срабатывает только в одной из двух очередностей. Итог: разговор после `/clear` уезжает в другую тему (например, из `#2` в `#1`), старая тема остаётся с 🏁. Лишней темы не появляется, но нарушен инвариант плана (PLAN_FINAL §2, `hub/registry.rs`, пункт "Слот для top-level сессии"), а тест `clear_in_one_process_stays_in_its_slot` покрывает только нереалистичный порядок. Воспроизведение: `scratch/crev/probe_registry_tests.rs::crev_clear_in_real_hook_order_jumps_topic`.
**Fix:** искать прежнюю сессию процесса не только в `pids`. Шаг 3 `allocate` должен найти слот той же папки, чей `current_session` имеет `entry.claude_pid == Some(pid)` на том же host, живой или `ended`. Проще всего предпочесть свободный слот, у которого последняя сессия была того же `host/claude_pid`. Переиспользование pid в Windows здесь безвредно: этот слот и так свободен. Добавить тест в порядке SessionEnd, потом SessionStart, при свободном меньшем ordinal. Существующий тест оставить для обратного порядка.

### I2 — major (латентный) — вложенный старт навсегда переписывает `kind` известной top-level сессии
`crates/cctg/src/hub/registry.rs:541-546, 574-575`.
`kind` сохраняется только при `parent_pid.is_none()`. Пусть известная top-level сессия B получает SessionStart с `parent_claude_pid = Some(pid другой сессии A)`, например `claude -p --resume B` из Bash-тула сессии A. Тогда `entry.kind` становится `Nested{A}`, а `entry.slot` слотом A. Следующий обычный интерактивный `--resume B` попадает в ветку "known session keeps what it was" и остаётся вложенной навсегда: своя тема B больше никогда не используется, сессия живёт в теме A. Для случая "предок = сама сессия" reviewer-2 закрыл ровно этот класс ранним возвратом без перезаписи, общий случай остался. Путь реален для TASK-019: headless resume через агент устройства, если агент является потомком claude-процесса, даст `Some(parent)`. Воспроизведение: `crev_nested_resume_of_other_toplevel_loses_its_slot_forever` (FAILED: slot `SlotId(0)` вместо `SlotId(1)`, kind `Nested{A}`).
**Fix:** известную `TopLevel`-сессию не переводить в `Nested`. Вложенный старт такой сессии должен вернуть `Parent(slot родителя)` для этого запуска и не трогать `kind`/`slot`/`pids` записи, как это уже сделано для self-pid. Нужен тест: nested resume, потом plain resume, и сессия возвращается в свой слот.

### I3 — minor — потерянный ответ на `createForumTopic` даёт тему-сироту
`crates/cctg/src/hub/slots.rs:379-382` и рестарт с Create в полёте (`registry.rs:716-719`, `busy` не сохраняется).
Если Telegram создал тему, а ответ потерялся (сетевой таймаут, ошибка транспорта) или хаб убит между отправкой и сохранением `topic_id`, слот получает `topic_failed`, и через `retry_every` уходит второй `Create`. Кроме того, `failed` сбрасывается при любой смене имени или иконки, так что при нестабильной сети сирот может быть несколько. В PLAN_FINAL §4 это записано только для разделителя, для Create нет. Полностью закрыть это нельзя, у `createForumTopic` нет ключа идемпотентности.
**Fix:** как минимум записать в известные ограничения. Дешёвое смягчение: для не-400 ошибок Create (сеть, 5xx) не повторять раньше `retry_every`, даже если имя или иконка изменились. Позже (TASK-017) можно сверяться с `forum_topic_created` из апдейтов по имени.

### I4 — minor — горячий цикл, если планировщик остановился
`crates/cctg/src/hub/slots.rs:357-362` и `pump` (`431-459`).
`delivery == None` ("the scheduler stopped") делает `release`, то есть `busy = false` без пометки `failed`. Следующий `pump` сразу снова выдаёт тот же job, `Outbox::submit` мгновенно возвращает закрытый oneshot, и цикл повторяется без паузы. Сейчас это возможно только после паники задачи `Scheduler::run`, потому что `Outbox` держит `dispatch_loop`, но тогда хаб уходит в 100% CPU вместо тихой деградации.
**Fix:** в ветке `None` вызывать `topic_failed` (разделитель при этом остаётся pending, retry идёт по тику) или хотя бы логировать один раз и не перевыдавать работу до `retry_every`.

### I5 — minor — ai-title перечитывается целиком на каждом Stop/UserPromptSubmit, пока его нет
`crates/cctg/src/hub/registry.rs:636-642`, `slots.rs:288-299`.
Пока `entry.title` пуст, каждый prompt-хук запускает полное чтение транскрипта до 256 MiB через `spawn_blocking`. У `claude -p` сессий ai-title может не появиться вовсе, а транскрипт растёт до десятков MiB. Дедупликация `reading` защищает только от параллельных чтений, не от повторных.
**Fix:** запоминать смещение, до которого файл уже просканирован (или размер файла при последнем скане), и читать дальше с него. Либо ограничить повторы, например не чаще раза в N минут на сессию.

### I6 — minor — сессия, продолжившая работу из другой папки, держит старый слот
`crates/cctg/src/hub/registry.rs:448-455` вместе с `is_free` (`397-403`).
Если известная сессия A стартует снова с другим `folder_key` (resume из другой папки), она получает слот новой папки. В старом слоте `current_session` остаётся A с `ended = false`, поэтому слот считается занятым, а его иконка повторяет состояние A: две темы показывают "жива" для одной сессии, пока не придёт SessionEnd. Случай редкий.
**Fix:** при переезде сессии в другой слот очищать `current_session` старого слота, если там всё ещё она.

## 4. Missing coverage

- `/clear` в порядке SessionEnd, потом SessionStart при свободном меньшем ordinal (I1).
- Вложенный resume известной top-level сессии и затем обычный resume (I2).
- Два быстрых перехода A→B→C, пока разделитель B не отправлен (например, слот `busy` правкой). Сейчас разделитель B молча заменяется разделителем C (`occupy` перезаписывает `pending_separator`, `registry.rs:504`). Поведение, видимо, приемлемое, но оно нигде не зафиксировано ни тестом, ни комментарием.
- `load` с дублирующимися `topic_id` или `(host, folder_key, ordinal)` в `registry.json`: не валидируется, `slot_by_topic` берёт первый. Тест или проверка в `load` не помешает.
- Ветка `delivery == None` в `on_topic_done` не покрыта тестом на уровне актора (I4).
- Create, упавший не 400-ошибкой, с последующей сменой имени: сколько `CreateTopic` уйдёт (I3).

## 5. Nits

- `topic_title` для пустого `cwd` даёт `"[host] "` с хвостовым пробелом (`folder_name("")` возвращает `""`). Telegram его, скорее всего, обрежет, но лучше подставлять `?`.
- `short()` продублирован в `registry.rs:172` и `slots.rs:91` вместе с константой `SHORT_ID`. Можно сделать `registry::short` `pub(crate)`.
- `IconError::TooFew(offered.len())` считает все предложенные id, включая предпочтительные. Текст "offered N icons" верен, но про нехватку запасных ничего не говорит.
- `can_delete = status == "creator" || ...`: бот не бывает `creator`, ветка мёртвая, хотя и безвредная.

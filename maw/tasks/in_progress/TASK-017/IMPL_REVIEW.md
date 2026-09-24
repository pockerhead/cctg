# TASK-017 IMPL_REVIEW (code-reviewer, claude opus, medium)

## 1. Verdict

**PASS**: все acceptance criteria выполнены и подтверждены кодом и тестами. Найдены две гонки уровня minor в логике Resume-заметки и пара дыр в покрытии. Флейк `hub::` воспроизведён и назван: это два существующих теста с фиксированными таймингами, продакшен-код задачи тут ни при чём.

## 0. Disconfirmation (сделано до оценки)

Контрпример, который сделал бы реализацию неверной: `entry.agent = None` в `SessionEnd` (registry.rs:878) ломает кого-то, кто читает `entry.agent` после конца сессии. Кандидаты: in-process `/resume`/`/clear` (тот же pid, тот же агент), повторная отправка вердиктов TASK-014, ответы TASK-022, stream TASK-016, иконки.

Результат проверки: **не подтвердился**.
- Все чтения `entry.agent` (grep по `crates/cctg/src/hub/*.rs`): `registry.rs:563` `state()` (для ended раньше срабатывает ветка Dead), `slots.rs:1346` `live_agent` и `slots.rs:1359` `live_reply_slot` / `slots.rs:1512` `stream_target`. Все три требуют `is_live_top_level` или `current_slot`, то есть живую сессию.
- Вердикты и ack TASK-014 идут по `conns`/`bound.session`, не по `entry.agent`. Промпты завершённой сессии и так закрываются.
- `/clear` и in-process `/resume`: `follow_pid` (slots.rs:616) переносит соединение по `conns` и `pids`, от `entry.agent` старой сессии не зависит. Покрыто веткой `clear` в `revival_by_resume_new_session_or_clear_delivers_once_in_order`.
- `agent_session` при регистрации после конца сессии: соединение привязывается к ended-сессии (`agent_connected` не проверяет `ended`). Это описанный residual (9), новых путей потери он не открывает.

## 2. Confirmed correct

- Буфер: `hub/buffer.rs` `push` вытесняет самое старое при 50 (`one_more_than_the_cap_drops_the_oldest`). `close` сбрасывает период целиком и возвращает заметку. Сериализация `#[serde(default)]` на всех полях, `skip_serializing_if = "Buffer::is_idle"` в `registry.rs:278`. Старый `registry.json` грузится, idle-слоты пишутся как раньше, `VERSION` остался 1 (`a_slot_buffer_survives_a_restart_and_an_older_file_loads_without_one`).
- Порядок: каждое текстовое сообщение сначала паркуется, потом делается `flush` (slots.rs `on_topic_message` → `park` → `flush`). Поэтому новое сообщение не обгоняет сохранённые, а полная очередь агента оставляет хвост на следующую попытку (`a_full_link_queue_keeps_the_rest_in_order_for_the_next_try`).
- Предупреждение о переполнении: одно за период, `overflow_told` ставится только если `send_messages` принял отправку. Следующий мёртвый период предупреждает снова (`the_51st_message_drops_the_oldest_and_warns_once_per_period`).
- Оживление: `flush_all` в начале каждого `pump` плюс `live_agent` (живая top-level текущая сессия с привязанным подключённым агентом). Проверены resume, новая сессия и `/clear`, у каждого пути доставка в том же слоте ровно один раз. Вложенные запуски и субагенты слот не оживляют: `nested_runs_and_subagents_never_revive_a_slot`, `registry::session_started` возвращает `Parent` для вложенного resume top-level id. Буфер одного слота не утекает в другой (`only_the_slot_that_revives_gets_its_kept_messages`).
- Агент прежнего запуска после resume ничего не получает (`kept_messages_never_go_to_the_agent_of_an_earlier_run`, registry.rs:878).
- Рестарт: `after_restart` буфер не трогает. Заметка с `message_id` переживает рестарт, повторной кнопки нет, после доставки сохраняется idle-буфер (`kept_messages_survive_a_restart_and_go_out_once`). E2E по настоящему TCP (`tests/buffer_e2e.rs`): `registered` и ровно три `inbound` по порядку, за 500 мс больше ничего.
- Кнопка Resume: `resume:<uuid>` занимает 43 байта, проверка `len <= MAX_CALLBACK_DATA` в байтах, токен `[A-Za-z0-9-]`. `resume:` проверяется в `press` раньше парсера разрешений, а `permissions::parse_callback("resume:…")` и так даёт `None`. Нажатие незнакомого пользователя отсекается раньше в `updates::classify` (allowlist по `from.id`). Устаревшее нажатие даёт `ANSWER_ALIVE` или `ANSWER_EXPIRED`. Ответы короче 200 символов. Клавиатура убирается через `Op::Edit` с `permissions::no_keyboard()`.
- Актор не ждёт Telegram: всё уходит через `hand_off` и `send_messages` по unbounded-каналу диспетчеру, кнопка учитывается в `MAX_QUEUED_MESSAGES` (`offer_resume` выходит при полном лимите, заметка при этом не ставится).
- Логи: `info!`/`debug!` содержат ordinal и короткий id, текста и user id в них нет. `tests/message_logs.rs` проверяет отсутствие текста и user id в логах и user id в `registry.json`. `Parked` user id не хранит.
- Изменённые старые тесты. `stale_or_foreign_presses_send_no_verdict`: для `resume:x` теперь `ANSWER_EXPIRED`, проверки "нет вердиктов, нет правок" остались. `a_burst_of_photos_gets_one_notice_a_minute`: троттлинг text-only проверяется как раньше, путь `OFFLINE_NOTICE` удалён вместе с кодом. `the_backlog_of_messages_for_telegram_is_capped` и `overflow_logs` переведены на фото, лимит проверяется тем же способом. Регрессий эти изменения не прячут.
- Крейты: `Cargo.toml` и `Cargo.lock` не менялись, в stdout ничего не пишется.

Прогоны (один `CARGO_TARGET_DIR` под `%TEMP%`, `-j 1`, `DEBUG=0`; каталог удалён):
- `cargo fmt --all --check`: ok. `cargo clippy --workspace --all-targets -D warnings`: ok.
- `cargo test --workspace --no-fail-fast`: lib 390 passed / 1 ignored, `buffer_e2e`, `message_logs`, `overflow_logs`, `stream_e2e` (11), `hook_cli` (7) и остальные бинарники ok.
- `hub::` 8 раз подряд: 8/8 ok.

## 3. Issues

### Minor 1: заметка Resume из прошлого периода может достаться новому, и текст "доставлено" появится в мёртвом слоте
`slots.rs` `on_resume_done` (фильтр `note.session == session && note.message_id.is_none()`).
Сценарий. Период 1: кнопка для A отправлена, ответа Telegram ещё нет. Сессия ожила, `flush` закрыл период: заметка без id, правки нет. A снова завершилась, пришло сообщение, `offer_resume` создал новую заметку `{A, None}` и отправил вторую кнопку. Первым приходит ответ на отправку из периода 1, его id записывается в заметку периода 2. Потом приходит ответ на вторую отправку, фильтр уже не проходит, и срабатывает `drop_resume_button`. В итоге самое свежее сообщение редактируется в `RESUMED_TEXT` ("снова на связи, доставлены"), хотя слот мёртв, а кнопка остаётся на старом сообщении. Окно равно времени ответа на отправку. Под 429 или при очереди из 20 сообщений в минуту это может быть десятки секунд.
Исправление: номер периода (или счётчик отправок) в `ResumeNote` и в `Work::Resume`/`Done::Resume`, сопоставлять по нему, а не по `session`.

### Minor 2: после смены текущей сессии слота заметка Resume остаётся со старой сессией
`slots.rs` `offer_resume` (условие `entry.buffer.resume.is_some()` → continue).
Мёртвый слот с сообщениями и кнопкой "resume A". Headless top-level `claude -p` в той же папке (без hub link, состояние NoChannel) или сессия без флага канала занимает слот и завершается. Кнопка A остаётся, нажатие даёт `ANSWER_EXPIRED` (A больше не текущая), новой кнопки для текущей мёртвой сессии нет до конца периода. Сообщения не теряются, но кнопка вводит в заблуждение.
Исправление: при расхождении `note.session` и `current_session` мёртвого слота убрать старую кнопку (`drop_resume_button`) и предложить новую.

### Minor 3 (тесты, не код задачи): флейк `hub::` назван
Под нагрузкой (4 параллельных процесса тестового бинарника, 9 раундов) падают:
- `hub::slots::tests::the_agent_calls_of_an_ended_session_are_forgotten` (slots.rs:6238, вместо "Late." приходит `итог не получен`). `drain_done` выходит после 200 мс тишины раньше, чем приходит `Done` чтения тела.
- `hub::slots::tests::one_slot_lives_through_hook_agent_end_and_the_next_session` (slots.rs:5406, в `registry.json` ещё A). Фиксированный `sleep(200 ms)` перед чтением снимка.

Оба теста написаны до TASK-017 и задачей не менялись. Сравнение на тех же условиях: бинарник `main` дал 1 падение (второй тест) на 36 прогонов, ветка 9 на 36 (оба теста). На ветке в бинарнике на 14 тестов больше, нагрузка выше, отсюда и частота. Логической связи с кодом задачи я не нашёл: первый тест вообще не вызывает `pump`. 60-секундного `WAIT` не увидел ни разу. Исправление (отдельной задачей или фиксером): ждать условие с таймаутом вместо фиксированного окна (`drain_done` до появления нужного pending, опрос `registry.json` как в `kept_messages_survive_a_restart_and_go_out_once`).

## 4. Missing coverage

- `on_resume_done` для периода, который уже закрыт: ветка `None => drop_resume_button` (ответ на отправку приходит после flush) нигде не проверяется. В тестах со `stalled_slots` `Done` не обрабатывается, в тестах с rig кнопка успевает получить id до оживления.
- Гонка из Minor 1 (две отправки в полёте, два периода).
- Переполнение, когда `send_messages` отказал из-за лимита: `overflow_told` не ставится, следующее вытеснение предупреждает снова. Не проверено.
- Живая сессия, у которой агент отключился после сохранения части буфера (Disconnected между двумя `flush`): должно прийти одно `QUEUED_NOTICE`, остаток ждёт. Не проверено.

## 5. Nits

- `buffer.rs` `resume_text` показывает полный id сессии в теме. Это нужно для `claude --resume`, приватности не нарушает, но стоит учесть, если в темы начнут пускать не только владельца.
- После первого уведомления `QUEUED_NOTICE` ("дойдут, когда она подключится") сообщения для живой сессии без флага канала молча копятся до 50 и уйдут только следующей сессии слота. Это соответствует спецификации (OPEN_DECISIONS), но слово "подключится" здесь обещает больше, чем будет.

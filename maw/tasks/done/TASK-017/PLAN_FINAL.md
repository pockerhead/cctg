# TASK-017 PLAN_FINAL: буфер мёртвого слота и кнопка Resume

## 1. Summary

Каждое текстовое сообщение allowlisted-пользователя в теме слота кладётся в буфер слота (`Slot.buffer` в `registry.json`) и в том же ходе актора отдаётся агенту живой top-level текущей сессии слота, если такой агент есть; иначе ждёт. Буфер держит 50 сообщений, 51-е вытесняет самое старое, предупреждение о переполнении печатается один раз за период. Живая сессия без агента получает одно `QUEUED_NOTICE` за период (старое `OFFLINE_NOTICE` удалено). Мёртвый слот с сохранёнными сообщениями получает одно сообщение с кнопкой `Возобновить` (`callback_data = resume:<session id>`, ≤ 64 байт); нажатие записывает `buffer.resume_asked` и отвечает, что запуск из Telegram ещё не подключён (TASK-019). Оживление слота это любая живая top-level сессия, ставшая текущей в слоте, чей агент привязан (resume той же сессии, новая сессия в свободном слоте, `/clear`); вложенные запуски, субагенты и headless-запуски без hub link слот не оживляют. Буфер уходит в исходном порядке, после чего период закрывается, а кнопка редактируется в текст без клавиатуры. Буфер переживает рестарт hub. Гарантия доставки: однократно в обычном ходе и после обычного рестарта; при аварии hub между передачей в очередь агента и сохранением снимка возможен дубль (at-least-once, принятый residual). Wire-протокол и агент не меняются.

Эталонная реализация, собранная и проверенная: `maw/tasks/in_progress/TASK-017/scratch/reviewer2/ws/` (HEAD + 7 файлов). Патч к текущему HEAD: `scratch/reviewer2/task017.patch`. Хэши: `scratch/reviewer2/hashes.txt`, проверка `scratch/reviewer2/verify_hashes.sh`.

## 2. Implementation steps

Шаг 0 (механический путь, рекомендуемый). Из корня репозитория на ветке `feature/dead-slot-buffer`:

```
git apply --check maw/tasks/in_progress/TASK-017/scratch/reviewer2/task017.patch
git apply maw/tasks/in_progress/TASK-017/scratch/reviewer2/task017.patch
bash maw/tasks/in_progress/TASK-017/scratch/reviewer2/verify_hashes.sh
```

Все 7 строк должны быть `OK` (хэши считаются по LF-байтам). Патч затрагивает только файлы ниже; ни одного другого файла менять не нужно. Шаги 1-7 описывают содержимое патча, чтобы его можно было проверить или воспроизвести вручную.

1. **Новый `crates/cctg/src/hub/buffer.rs`** (чистый модуль, без IO).
   - `pub const MAX_BUFFERED: usize = 50`; тексты `QUEUED_NOTICE`, `OVERFLOW_NOTICE`, `RESUMED_TEXT`, `ANSWER_UNAVAILABLE`, `ANSWER_ALIVE` (русские, фиксированные; ответы на кнопку ≤ 200 символов).
   - `Parked { message_id: i64, thread_id: i64, text: String, reply_to: Option<i64> }` (serde, `reply_to` пропускается при `None`). Telegram user id не хранится: `updates::classify` отбрасывает его до актора.
   - `ResumeNote { session: String, message_id: Option<i64> }`.
   - `Buffer { messages: VecDeque<Parked>, overflow_told, queued_told, resume: Option<ResumeNote>, resume_asked }`, все поля `#[serde(default)]`; `is_idle()` (== `Default`), `push(Parked) -> bool` (true: вытеснено старейшее), `close() -> Option<ResumeNote>` (сброс в default, возвращает заметку).
   - `resume_text(session)` (короткий id, лимит, строка `claude --resume <id>`, обрезка до 4096 через `registry::cut`), `callback_data(session) -> Option<String>`, `parse_callback(&str) -> Option<&str>`, `keyboard(String) -> Value`. Токен: непустой, только `[A-Za-z0-9-]`, `"resume:".len() + id.len() <= permissions::MAX_CALLBACK_DATA` (байты).
   - Unit-тесты: вытеснение старейшего; `close` сбрасывает всё и отдаёт заметку; round trip данных, ≤ 64 байт, `resume:` не парсится как вердикт, `allow:`/`deny:`/мусор не парсятся как resume, 57-байтный id влезает, 58-байтный и не-ASCII нет; тексты влезают в toast; `{}` грузится как idle, полный буфер делает round trip.
   Причина: чистая логика отдельно от актора, как `permissions.rs`.

2. **`crates/cctg/src/hub/mod.rs`**: `pub mod buffer;` (публично: интеграционные тесты импортируют тексты).

3. **`crates/cctg/src/hub/registry.rs`**:
   - `use super::buffer::Buffer;`
   - В `Slot` после `pending_separator`: `/// Topic messages no session of the slot could take yet (TASK-017).` `#[serde(default, skip_serializing_if = "Buffer::is_idle")] pub buffer: Buffer,` — старые файлы грузятся, idle-слоты пишутся как раньше, `VERSION` остаётся 1.
   - В литерале `Slot { .. }` внутри `allocate`: `buffer: Buffer::default(),`.
   - `pub fn slot_mut(&mut self, id: SlotId) -> Option<&mut Slot>` рядом с `slot()`; `dirty` выставляет вызывающий.
   - `after_restart` буфер не трогает (сохраняются сообщения, заметка, отметки).
   - **Изменение ревью 2: завершённая сессия теряет привязку агента.** В `apply_hook`, ветка `HookEvent::SessionEnd`, после `entry.waiting = false;` добавить:

     ```rust
                     // The run's agent goes with it: a resume starts a new claude
                     // and a new agent, and the old link, still open for a moment,
                     // must not take the slot's kept messages (TASK-017). After
                     // `/clear` the agent follows its pid (`Slots::follow_pid`).
                     entry.agent = None;
     ```

     Ветку "A reused pid" в `session_started` не трогать: там процесс прежнего запуска давно умер, его связь закрыта. Причина: `session_started` при resume известной сессии ставит `ended = false`, но оставлял `entry.agent` прежнего запуска. Пока TCP-связь того агента ещё не закрыта (SessionEnd приходит раньше, чем умирает процесс), `live_agent` считал его живым, и `flush` отдал бы в очередь умирающего агента весь буфер (до 50 сообщений), они бы пропали; иконка при этом показывала Alive вместо NoChannel. Для завершённой сессии `entry.agent` больше нигде не нужен: `live_agent`, `live_reply_slot` и `stream_target` требуют живую сессию, `state()` для ended даёт Dead до проверки агента. `/clear` не затронут: `follow_pid` работает по `conns`, не по `entry.agent`. SessionEnd вложенного `--resume` с чужим pid registry игнорирует раньше, привязка остаётся. Сравнение `claude_pid` агента и сессии как альтернатива отвергнуто: агент и хук вычисляют pid независимо (`current_lineage(None, None, "")` против env-варианта хука), при расхождении inbound пропал бы навсегда; это воспроизвелось падением `stream_e2e::e2e_reactions`.
   - Тест `a_slot_buffer_survives_a_restart_and_an_older_file_loads_without_one`.

4. **`crates/cctg/src/hub/slots.rs`, production.**
   - Модульная документация: сообщение, которое никто не может принять, ждёт в слоте и уходит первой живой top-level сессии слота с привязанным агентом; мёртвый слот показывает одну кнопку Resume; throttling `notice_every` теперь только для text-only.
   - Импорты `super::buffer::{self, Parked, ResumeNote}`, `SlotState`.
   - Удалить `OFFLINE_NOTICE`; документация `Options::notice_every` про фото.
   - `Work::Resume { slot, session }` и `Done::Resume { slot, session, delivery }`, связка в `dispatch_loop`, `on_done` вызывает `on_resume_done`.
   - `on_topic_message`: проверки thread/slot/text и text-only notice без изменений, затем `self.park(slot, Parked { .. })` и `self.flush(slot)`.
   - `park`: до push вычисляет `offline = live_agent(slot).is_none()` и `dead = state == Dead`; push; `dirty`; info-лог без текста; одно `OVERFLOW_NOTICE` за период (`overflow_told` ставится только если `send_messages` принял); одно `QUEUED_NOTICE` за период при `offline && !dead`.
   - `flush`: при `live_agent(slot)` отдаёт сообщения с головы через `try_send`, при первом отказе останавливается (остаток ждёт следующего `pump`); после успешной передачи `pop_front`, `dirty`, stream receipt, реакция 👀, прежний info-лог. Пустой не-idle буфер закрывает период (`close`), кнопка редактируется в `RESUMED_TEXT` с пустой клавиатурой.
   - `inbound(session, &Parked)`: прежний код meta (`chat_id`, `message_id`, `thread_id`, `reply_to_message_id`, `target_agent` пересчитывается для доставляющей сессии).
   - `flush_all()` в начале каждого `pump` для слотов с непустым буфером; `offer_resume()` после него: мёртвый слот с сохранёнными сообщениями, темой и без заметки получает одно сообщение с кнопкой; заметка ставится до отправки (не больше одного раза за период, в том числе через рестарт); учитывается `MAX_QUEUED_MESSAGES`.
   - `drop_resume_button(message_id)`: `Op::Edit { text: RESUMED_TEXT, reply_markup: Some(permissions::no_keyboard()) }` через `Work::Callback`, одна попытка.
   - `on_resume_done`: освобождает место в очереди сообщений; `Sent` id записывается в совпадающую заметку, иначе (период уже закрыт) кнопка убирается сразу; ошибка отправки: один warn.
   - `press_resume(session)` и в начале `press`: `resume:<id>` проверяется до парсера разрешений. Живая top-level сессия: `ANSWER_ALIVE`; завершённая текущая сессия своего слота: `resume_asked = true` (сохраняется), `ANSWER_UNAVAILABLE`; иначе `permissions::ANSWER_EXPIRED`.
   - `live_agent` не меняется: живая top-level текущая сессия слота с привязанным и подключённым агентом.

5. **`crates/cctg/src/hub/slots.rs`, тесты.** Хелперы `contents`, `buffered`, `end`, `connect_queue`, `drain`, `resume_sends`. Новые тесты:
   - `a_message_nobody_can_take_now_is_kept_and_told_once` (заменяет `a_message_nobody_can_take_gets_one_notice_each`);
   - `the_51st_message_drops_the_oldest_and_warns_once_per_period`;
   - `a_full_link_queue_keeps_the_rest_in_order_for_the_next_try`;
   - `revival_by_resume_new_session_or_clear_delivers_once_in_order`;
   - `nested_runs_and_subagents_never_revive_a_slot`;
   - `kept_messages_never_go_to_the_agent_of_an_earlier_run` (ревью 2, проверяет изменение registry): старый агент A (pid 10) ещё подключён, A завершилась, три сообщения сохранены, `SessionStart(resume)` A с pid 12, `pump`, четвёртое сообщение: старый агент ничего не получает, буфер `[0,1,2,3]`; агент pid 12 получает `[0,1,2,3]`;
   - `only_the_slot_that_revives_gets_its_kept_messages` (ревью 2): два слота одной папки, #1 мёртв с тремя сообщениями, #2 живой с агентом; агент #2 получает только своё сообщение; новая сессия C занимает слот #1 (новый слот не создаётся), её агент получает `[0,1,2]`, агент #2 ничего;
   - `a_dead_slot_shows_one_resume_button_that_records_the_wish`;
   - `kept_messages_survive_a_restart_and_go_out_once` (обычный рестарт после сохранённого снимка; окно аварии между передачей и сохранением этим тестом не покрывается и не обещается).
   Изменённые по спецификации существующие тесты: `an_ended_session_gets_no_inbound_even_with_its_agent_still_linked` (ждёт одно сообщение Resume вместо `OFFLINE_NOTICE`, inbound по-прежнему нет); `stale_or_foreign_presses_send_no_verdict` (`resume:x` теперь `ANSWER_EXPIRED`, вердиктов и правок по-прежнему нет); `a_burst_to_a_dead_slot_gets_one_notice_a_minute` → `a_burst_of_photos_gets_one_notice_a_minute`; `the_backlog_of_messages_for_telegram_is_capped` (наполняет лимит фото); `a_stalled_telegram_never_stalls_inbound` (только комментарии).

6. **Новый `crates/cctg/tests/buffer_e2e.rs`**: настоящий `serve_agents` на `127.0.0.1:0`, настоящие `Slots` и `Scheduler`, fake transport, сырой wire-пир по TCP. Старт, конец сессии, три сообщения, ожидание кнопки, `SessionStart(resume)`, затем hello + register: пир получает `registered` и ровно три `inbound` по порядку (`message_id` 1, 2, 3), кнопка редактируется в `RESUMED_TEXT`, за 500 мс больше ничего.

7. **`crates/cctg/tests/message_logs.rs` и `tests/overflow_logs.rs`**: сообщение до агента сохраняется (`QUEUED_NOTICE`), `registry.json` содержит текст, но не user id; агент получает сначала сохранённое, потом живое; ожидаемые строки логов "message kept for the slot until a session is on line" и "kept messages handed to the slot's session"; негативные проверки приватного текста и user id остаются. В `overflow_logs` сообщения это фото (`text: None`), по-прежнему одно предупреждение переполнения за эпизод.

## 3. Test plan

Сборка на этом хосте: один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз, каталог удалить в конце.

1. Итерации: `cargo test -j 1 --offline -p cctg --lib hub::` → 294 passed, 0 failed, 1 ignored. Полный lib: 390 passed, 1 ignored.
2. `cargo test -j 1 --offline -p cctg --test buffer_e2e --test message_logs --test overflow_logs --test stream_e2e` → все ok.
3. Финально: `cargo fmt --all --check`, `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings`, `cargo test -j 1 --offline --workspace --no-fail-fast` → exit 0. Эталонный вывод: `scratch/reviewer2/workspace_test.txt`.
Флейки, замеченные на загруженном хосте: `one_slot_lives_through_hook_agent_end_and_the_next_session` и `a_nested_answer_survives_a_restart_before_its_end` (существующие тесты TASK-011/015 с фиксированными `sleep` 200-300 мс перед чтением `registry.json`, кодом задачи не затронуты) и три прогона `hub::` из десяти сразу после полного прогона workspace, упавшие по 60-секундному `WAIT` (имя теста не записано). Последние не воспроизвелись в 57 следующих прогонах (25 через cargo, 12 в три параллельных процесса, 20 подряд бинарником). Если QA увидит 60-секундный таймаут, первым делом снять имя теста; флейк `a_dead_slot_shows_one_resume_button_that_records_the_wish` (2 из 12 в эталоне планировщика) закрыт изменением `registry.rs`.
4. Мутации (`scratch/reviewer2/mutations.py`, результат `mutations.out.txt`): 14 из 14 убиты: M1-M12 планировщика (M5 переписан под исходный `live_agent`) плюс M13 (конец сессии не снимает привязку агента: убит `kept_messages_never_go_to_the_agent_of_an_earlier_run` и `a_dead_slot_shows_one_resume_button_that_records_the_wish`) и M14 (сообщение кладётся в чужой слот: убит `only_the_slot_that_revives_gets_its_kept_messages` и тесты TASK-021).

Соответствие acceptance:
- буфер в мёртвом слоте, тема не закрыта, иконка dead: `an_ended_session_gets_no_inbound_even_with_its_agent_still_linked`, `a_dead_slot_shows_one_resume_button_that_records_the_wish` (нет `Delete`, нет close-операции в `Op`), `nested_runs_and_subagents_never_revive_a_slot` (`SlotState::Dead`);
- 51-е вытесняет старейшее, одно предупреждение за период: `the_51st_message_drops_the_oldest_and_warns_once_per_period`, `buffer::tests::one_more_than_the_cap_drops_the_oldest`;
- оживление в исходном порядке один раз, любые пути, без вложенных/субагентов: `revival_by_resume_new_session_or_clear_delivers_once_in_order`, `only_the_slot_that_revives_gets_its_kept_messages`, `kept_messages_never_go_to_the_agent_of_an_earlier_run`, `nested_runs_and_subagents_never_revive_a_slot`, `a_full_link_queue_keeps_the_rest_in_order_for_the_next_try`, `buffer_e2e`;
- рестарт без дубля: `kept_messages_survive_a_restart_and_go_out_once`, registry-тест round trip;
- callback ≤ 64 байт и понятный ответ: `buffer::tests::resume_data_fits_round_trips_and_never_looks_like_a_verdict`, `a_dead_slot_shows_one_resume_button_that_records_the_wish`;
- нет user id в логах и персисте: `tests/message_logs.rs`;
- существующие тесты: полный прогон workspace.

## 4. Rollout notes

- Миграций нет: `registry.json` VERSION 1, поле `buffer` опционально и не пишется в idle-состоянии. Откат на старый бинарь: старый hub проигнорирует неизвестное поле `buffer` только если его serde не `deny_unknown_fields` (не используется), сохранённые сообщения при этом теряются.
- Новых env-переменных и флагов нет. Wire не меняется (`wire::VERSION` тот же), агент не меняется.
- Поведение для пользователя: пропадает "не доставлено"; живая сессия без агента получает одно "сохранены" за период; мёртвый слот получает сообщение с кнопкой при первом сохранённом сообщении, не при каждом выходе сессии.
- Residuals (документированы): (1) авария hub между `try_send` и сохранением снимка даёт повтор после рестарта (at-least-once); (2) разрыв связи после приёма в очередь агента теряет сообщение (как у живого пути сейчас); (3) существующее окно TASK-009: offset сохранён раньше снимка, авария теряет update; (4) потерянный ответ на отправку кнопки оставляет кнопку без id, её не редактируют; (5) правка клавиатуры в конце периода одна попытка, устаревшая кнопка отвечает корректно (`ANSWER_ALIVE`/`ANSWER_EXPIRED`); (6) до 50 текстов на давно мёртвый слот лежат в локальном gitignored `registry.json`, слоты не чистятся; (7) агент сессии без channel-флага принимает сообщения, Claude Code их молча дропает (факт channel-домена); (8) при `/clear` с уже сохранёнными сообщениями может мелькнуть кнопка, которую тут же убирает flush; (9) если агент завершённого запуска переподключится (обрыв связи) до смерти процесса, он снова привяжется к ended-сессии и при немедленном resume может забрать буфер; окно порядка долей секунды, отдельной защиты нет.
- TASK-019 читает `buffer.resume_asked` и `buffer.resume.session`. Если TASK-019 захочет показывать кнопку сразу при конце сессии, это вызов `offer_resume` для слота из `on_hook` плюс правка тестов с точными потоками операций (см. review notes, finding 1).

## 5. Review notes

**Disconfirmation.** Проверенный контрпример против PLAN_V2: его исправление finding 1 (кнопка на каждом `SessionEnd`) добавляет сообщение в тему при каждом выходе claude и ломает существующие тесты других задач. Подтвердилось кодом: попытка (`scratch/reviewer2/slots_fix1_attempt.rs`) уронила 6 тестов TASK-011/014/015/016/022, все из-за лишнего `Send` после конца сессии. Второй контрпример, найденный по ходу: resume той же сессии при ещё открытой связи прежнего агента. Подтвердился как флейк эталона (`a_dead_slot_shows_one_resume_button_that_records_the_wish` падал 2 раза из 12: сообщения ушли агенту прежнего запуска) и исправлен в `registry.rs` (конец сессии снимает привязку агента). Первая версия исправления (сравнение pid в `live_agent`) уронила `stream_e2e::e2e_reactions` и отвергнута, см. шаг 3.

Разбор findings PLAN_V2:
1. Кнопка только после первого сообщения: воспроизводится, **отклонено**. До TASK-019 кнопка ничего не запускает и только говорит "возобновите в терминале"; показывать её на каждом выходе это шум в группе (20 сообщений/мин общие) и правка 6 чужих тестов. Спецификация выполняется: в мёртвом слоте, куда пишут, кнопка появляется. Как включить раньше, описано в rollout для TASK-019.
2. Callback не привязан к message_id/периоду: **отклонено**. id сессии это UUID, коллизий как у 5-буквенных id разрешений нет; старая кнопка той же сессии означает то же намерение; кнопка чужой сессии даёт `ANSWER_EXPIRED`; gate по allowlist стоит до разбора data.
3. Правка клавиатуры без повторов: воспроизводится, **отклонено**: та же политика, что у правки истёкшего prompt в TASK-014 (одна попытка через `Work::Callback`), устаревшая кнопка отвечает корректно. Residual (5).
4. Смешение dead-периода и периода буфера: следствие finding 1, отклонено вместе с ним.
5. `flush_all` на каждом `pump`: **отклонено**, это O(числа слотов) без await, как `topic_work` в том же `pump`; слот без агента выходит сразу.
6. Запись реестра на каждое живое сообщение: **отклонено**. Push и pop происходят в одном ходе, снимок делается после хода, текст на диск не попадает; остаётся один лишний save, как у stream receipt.
7. Restart-тест покрывает только спокойный случай: принято как формулировка, код не меняется; гарантия сужена до at-least-once при аварии (rollout, residual 1).
8. Недостающие тесты: добавлены `only_the_slot_that_revives_gets_its_kept_messages` и `kept_messages_never_go_to_the_agent_of_an_earlier_run`. Тест stranger-callback с `resume:` не нужен: `updates::classify` проверяет allowlist до чтения `data`, существующий тест с `allow:abcde` покрывает тот же путь. Fairness-тесты не нужны (finding 5 отклонён).
9. Изменённые существующие тесты: проверены по диффу, каждое изменение следует из удаления `OFFLINE_NOTICE`; проверки вердиктов, правок и text-only throttling сохранены.
10. База эталона: подтверждено, между `7cdaa99` и HEAD только файлы задачи; патч ревью 2 собран против текущего HEAD.

Изменения кода относительно эталона планировщика: одна строка `entry.agent = None;` (ветка `SessionEnd` в `registry.rs`) и два теста в `slots.rs`. PCTX-предложение планировщика код не меняет, оставлено оркестратору; к нему стоит добавить, что агент прежнего запуска resumed-сессии не получает inbound.

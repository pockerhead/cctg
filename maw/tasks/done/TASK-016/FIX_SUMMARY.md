# TASK-016 FIX_SUMMARY (fixer)

Проверка перед правками: самое конкретное утверждение ревью, которое могло бы сломать корректный код при дословном исполнении, это M1 «если `waiting.back()` уже `Barrier`, заменить его». Проверил по `hub/stream.rs`: `advance()` снимает голову, пока там `Accepted`-сообщение или барьер, и возвращает последний пройденный барьер. Между двумя соседними барьерами ничего нет, поэтому `advance` всегда проходит их вместе и возвращает второй. `rewind` берёт offset из реестра, а не из `waiting`, `stuck`/`unanswered` смотрят только на сообщения. Замена эквивалентна, утверждение верное. Рост тоже реальный: `on_chunk` зовёт `live.barrier(read_to)` на каждый ответ, в том числе на пустой, а `pump_streams` читает, пока `unanswered() < 64`.

## 1. Fixed

- **M1 (major), `Live.waiting` растёт на барьер за каждое холостое чтение.** `Live::barrier` заменяет последний элемент, если это барьер, иначе добавляет новый (`crates/cctg/src/hub/stream.rs`). Барьеров теперь не больше, чем сообщений, плюс один. Тест `idle_reads_while_a_message_waits_keep_one_barrier`: одно ждущее сообщение и 201 холостое чтение дают `waiting.len() == 2`. `advance()` возвращает `None`, пока сообщение ждёт, а после accept отдаёт последний `to` (209) и снимок calls из последнего барьера. Два сообщения с барьерами между ними дают 4 элемента и коммитятся по очереди: 220, потом 230.
- **m2, WARN «session transcript not found» на старте каждой новой сессии.** Проблема подтвердилась. Claude Code создаёт jsonl только на первом промпте: у 60 свежих локальных транскриптов время создания файла совпадает с timestamp первой записи в пределах ~2 с (`scratch/fixer/transcript_birth_probe.py`, вывод рядом). Добавил `Live.file_seen`, он ставится при первом ответе не-`missing` и переживает `rewind`. Если файла нет, а поток его ещё не видел и читает с байта 0, пишется одна строка DEBUG «session transcript not written yet». Во всех остальных случаях остаётся один WARN «session transcript not found» (`hub/slots.rs`, ветка `missing` в `on_chunk`). Тест `tests/stream_logs.rs` проверяет: до появления файла одна DEBUG-строка и ноль WARN. После того как файл прочитан и удалён, ровно один WARN. Проверки приватности (путь, текст) на месте.
- **nit, `[Request interrupted by user]` уходил как `> prompt` и считался `NewTurn`.** Теперь он идёт как `StreamEvent::Note`: простой текст, завершённые вызовы уходят (flush), нового хода нет. После прерывания `Stop` не приходит, так что границу хода даст следующий настоящий промпт. `render::INTERRUPT_PREFIX` стал `pub(crate)`, то же правило ловит и вариант `... for tool use]`. Тесты в `crates/transcript/tests/stream.rs`: строка из фикстуры `stream.jsonl` и реальная форма варианта с tool use (массив с `text`-блоком, форму сверил по локальному транскрипту). Решение записано в `log.jsonl`. `/brief` не трогал, там прерывание по-прежнему `> [...]` и маркер завершения.
- **Нехватка тестов:**
  - граница барьеров: см. M1;
  - `a_chunk_for_another_session_on_the_connection_is_dropped` (`hub/slots.rs`): чанк с `session_id` B на соединении сессии A отбрасывается. Чтение A всё ещё ждёт, записи для B нет, в очереди ничего, `read_at` 0. Тот же чанк для A принимается;
  - `a_stream_message_telegram_refuses_with_a_4xx_is_skipped_and_the_offset_moves`: ответ 400 (не про пропавшую тему) засчитывается как принятый. Offset в реестре уходит с 0 на 10, `queued_messages` возвращается к 0, `stuck()` ложно, поэтому повторной отправки нет.
- **m1, m3, ✓ у `Agent`, запись 64 MiB:** код не трогал, как велел оркестратор. Каждому пункту одна строка в новом разделе «5. Известные ограничения» в `IMPL_SUMMARY.md`.

## 2. Skipped

- Остальное из «Missing coverage» ревью (`StreamItem::Other` в чанке на стороне hub, `on_turn_answer` до создания `Live`): в объём этого раунда не входило. Поведение описано в ревью как корректное.
- Совет ревью не читать совсем, пока очередь на паузе: после слияния барьеров он не нужен (так пишет и само ревью). Иначе строки и удержанные ответы ждали бы за общей очередью 20/мин.

## 3. Test results

Один `CARGO_TARGET_DIR=%TEMP%/cctg-fix016-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз. После прогона каталог удалён.

- `cargo fmt --all --check`: rc=0 (`scratch/fixer/fmt.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: rc=0 (`scratch/fixer/clippy.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0 (`scratch/fixer/workspace_test.txt`). `cctg` lib 361 passed / 1 ignored (было 358: +3 новых теста), `stream_logs` 1, transcript `tests/stream.rs` 4, остальные бинари зелёные.

Изменённые файлы: `crates/cctg/src/hub/stream.rs`, `crates/cctg/src/hub/slots.rs`, `crates/cctg/tests/stream_logs.rs`, `crates/transcript/src/render.rs`, `crates/transcript/src/stream.rs`, `crates/transcript/tests/stream.rs`, `maw/tasks/in_progress/TASK-016/IMPL_SUMMARY.md`. Ничего не закоммичено.

# Round 2 (QA NO_SHIP)

Проверка перед правками. Самое конкретное предложение QA, которое при дословном исполнении сломало бы рабочий код: M2, "stop sending a session's later stream messages once one is refused". Если сделать это в акторе как одно потоковое сообщение в полёте на сессию, в очереди планировщика никогда не окажется двух строк одной темы. Тогда `merge_lines` нечего склеивать и ломается критерий 8 (склейка при упоре в лимит). Проверил по `hub/scheduler.rs`: `merge_lines` берёт только строки, уже стоящие в `self.message` за головой, так что утверждение верное. Поэтому M2 решён в планировщике, а не в акторе. B1 и M1 подтвердились чтением кода: `serve_agents` пересылал только `Reply | PermissionRequest | PermissionAck`, а `read_chunk` при `from: None` начинал с `len`, даже если это середина строки.

## 1. Fixed

- **B1 (blocker), ingress выкидывал `transcript_chunk`.** В `hub/ingress.rs` в пересылаемую ветку добавлен `AgentMsg::TranscriptChunk { .. }`. Других новых вариантов `AgentMsg` в задаче нет: `wire.rs` добавил только `TranscriptChunk`, а `HubMsg::TranscriptRead` идёт в другую сторону. e2e QA перенесён в репозиторий как `crates/cctg/tests/stream_e2e.rs`. Там настоящий бинарь `cctg agent` (`CARGO_BIN_EXE_cctg`, временные `CLAUDE_CONFIG_DIR`/`USERPROFILE`/`HOME`, stderr в null), настоящий `serve_agents` по TCP на `127.0.0.1:0`, настоящие `Slots` и `Scheduler` и фейковый Telegram. 6 тестов: порядок и частичная строка, отказы 502, рестарт hub, реакции, склейка под лимитом, resume на оборванном конце. Тест QA, читавший реальные транскрипты из `~/.claude/projects`, не перенесён: он зависит от машины. `refused` теперь проверяет точный порядок (`got == want`), а не только порядок первых появлений. Прогон около 3.6 с, 5 повторов подряд зелёные.
- **M1, resume на оборванном конце переигрывал историю.** `tail.rs`: при `from: None` старт берётся из `last_line_start`. Это позиция после последнего `\n` до `len`, найденная обратным сканом блоками по 64 KiB. Если перевода строки нет вовсе, старт 0; при ошибке чтения остаётся `len`, как было. Недописанная строка уходит одним куском, когда допишется, а история до неё не переотправляется. Тесты: `no_offset_at_a_torn_end_starts_at_that_line_and_never_before_it` (оборванная строка длиннее блока скана) и `no_offset_in_a_file_without_a_newline_starts_at_its_start`. e2e `e2e_resume_at_a_torn_end_does_not_replay_history` проходит. Doc `HubMsg::TranscriptRead` уточнён.
- **M2, после отказа следующие строки обгоняли отказанную.** `Op::Stream` получил поле `restart`. Если потоковая строка получает ошибку, отличную от Telegram 4xx (4xx актор засчитывает как пропуск), планировщик помечает тему как сломанную. Строки этой темы, стоящие в очереди до следующей `restart`-строки, и все, что приходят позже без `restart`, отбрасываются неотправленными. Их receiver закрывается, актор видит `Refused` и после `stuck` делает rewind. `Live::new` ставит `restart = true`, так что первое сообщение потока, в том числе после rewind, снимает поломку. Другие темы не затрагиваются. Тесты: `scheduler::after_a_refused_line_its_topic_sends_nothing_until_a_restart_line` (включая строку, пришедшую уже после ответа об отказе), `scheduler::a_line_telegram_rejects_with_a_4xx_does_not_break_its_stream`, `slots::lines_after_a_refused_stream_message_never_show_before_it` и e2e `refused`. Старый тест `a_refused_merged_message_answers_none_of_its_lines_as_merged` обновлён под новое поведение: строки после отказанного сообщения тоже без ответа и не отправляются. Решение записано в `log.jsonl`, остаточный эффект при ротации слота добавлен в `IMPL_SUMMARY.md`.
- **Потеря `ends_unclaimed` (наблюдение QA).** Проблема реальная, но окно узкое. Хук `Stop` доходит до очереди актора раньше, чем Claude пишет следующий промпт. Но `select!` актора выбирает случайную готовую ветку, а `Stop` и чанк идут по разным каналам. Поэтому чанк с `TurnEnd` и следующим промптом может обработаться раньше `Stop`. Бывает это при занятом акторе или при POST хука, который пришёл с опозданием. Раньше `NewTurn`/`Working` обнуляли счётчик, и ответ ждал конца следующего хода (выходил после его строк) или 5 с. Теперь `Live::lapse_ends` при новом ходе не обнуляет незабранные концы, а даёт им срок `hold_answer`, и `Live::claim_end` забирает их до этого срока. Тесты: `slots::a_stop_that_comes_after_the_next_prompt_was_read_still_takes_its_turn_end` (до правки ответ держался 30 с) и `stream::turn_ends_read_before_a_new_turn_stay_claimable_until_they_lapse`. Компромисс записан в `log.jsonl` и `IMPL_SUMMARY.md`.

## 2. Skipped

- Открытый вопрос QA о форме channel-сообщения посреди хода (`queued_command` со списком в `prompt`) проверяется только живой сессией. Оставлен на живую проверку после merge, как в OPEN_DECISIONS.
- Критерий 5 (ротация сессии) e2e-тестом не покрыт, остались юнит-тесты. В объём раунда это не входило.

## 3. Test results

Один `CARGO_TARGET_DIR=%TEMP%/cctg-fix2-016-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, один cargo за раз. Каталог удалён.

- `cargo fmt --all -- --check`: rc=0 (`scratch/fixer/fmt2.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: rc=0 (`scratch/fixer/clippy2.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0 (`scratch/fixer/workspace_test2.txt`). `cctg` lib 368 passed / 1 ignored (было 361, +7 тестов), `stream_e2e` 6, `stream_logs` 1, остальные бинари зелёные.
- `stream_e2e` 5 повторов подряд: 6/6 каждый раз (`scratch/fixer/stream_e2e_repeat.txt`).
- Проверка на невакуумность (`scratch/fixer/mutations_round2.py`, вывод `mutations_round2.out.txt`): 7/7 мутаций, откатывающих правки, убиты нужными тестами (B1 через e2e, M1 через unit и e2e, M2 через slots, e2e и 4xx-тест планировщика, `ends_unclaimed` через slots). Дерево после прогона восстановлено.

Изменённые файлы: `crates/cctg/src/hub/{ingress,scheduler,slots,stream}.rs`, `crates/cctg/src/tail.rs`, `crates/cctg/src/wire.rs` (только doc), новый `crates/cctg/tests/stream_e2e.rs`, `IMPL_SUMMARY.md`. Ничего не закоммичено.

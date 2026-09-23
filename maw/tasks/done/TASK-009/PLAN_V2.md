# PLAN_V2 — TASK-009: hub — local transcript commands

## 1. Review notes

### Что проверено

- Обязательный disconfirmation-контрпример: top-level `<uuid>.jsonl` является symlink на субагентский транскрипт или файл вне projects root. Контрпример **не подтвердился**: reference использует `DirEntry::metadata()`, а этот метод не следует по symlink; `Metadata::is_file()` для ссылки ложен. Кроме того, `ProjectsDir` читает только прямых детей прямых project-каталогов. Это соответствует [официальной документации Rust](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata); менять фильтр из-за symlink не нужно.
- Baseline из корня репозитория независимо собран в `%TEMP%/cctg-task009-reviewer1-baseline-target`: `cargo test --workspace --offline` — 110 passed, 1 ignored.
- Reference из `scratch/planner/ws` независимо проверен в другом target-каталоге: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, `cargo test --workspace --offline` — чисто, 132 passed, 1 ignored. Все 13 SHA-256 из `hashes.txt` совпали; `proto.diff` применим к HEAD; `Cargo.lock` добавляет только локальную зависимость `transcript` к `cctg`.
- Один target-каталог нельзя переиспользовать между корнем и workspace-копией с одинаковыми package names: Cargo может повторно использовать тестовые бинарники другой копии. Исходный план этого не предупреждает; Step 0/проверки должны использовать разные target dirs.
- Bot API проверен по первоисточнику: update подтверждается вызовом `getUpdates` с offset выше `update_id`; `sendMessage.text` ограничен 1–4096 символами; `sendDocument` принимает multipart upload до 50 MB, caption — до 1024 символов. См. [getUpdates](https://core.telegram.org/bots/api#getupdates), [sendMessage](https://core.telegram.org/bots/api#sendmessage), [sendDocument](https://core.telegram.org/bots/api#senddocument).

### Недостатки исходного плана

1. **Ответ не совпадает с выводом библиотеки.** В `commands.rs::prepare` reference делает `Reply.text = header + "\n\n" + body`, а `replies_match_the_library_on_fixtures` ожидает тот же самостоятельно сконструированный wrapper. Поэтому тест зелёный, хотя отправленный текст и байты документа не равны `render_brief(last_prompts(...))` / `render_full(last_prompts(...))`, как требует acceptance criterion. Заголовок — незапрошенное изменение рендера. Исправление: тело ответа и файла должно быть ровно библиотечным результатом; идентификатор выбранной сессии остаётся в безопасном логе и, для документа, может быть коротким caption/именем файла.
2. **Ошибка сохранения offset нарушает заявленный at-most-once контракт.** `updates::poll` присваивает новый in-memory offset, при ошибке `OffsetStore::save` лишь пишет warning и всё равно передаёт batch обработчикам. После рестарта этот batch может прийти снова и повторить команду. Исправление: не менять рабочий offset и не dispatch-ить batch, пока новый offset не сохранён; retry сохранения с backoff, затем dispatch ровно один раз. Краш после успешного save, но до/во время dispatch по-прежнему сознательно теряет необработанную часть batch — это цена at-most-once, её надо явно зафиксировать.
3. **Тест fallback проверяет операции, но не общий поток данных.** Нужна проверка, что успешно доставленные чанки плюс документ с недоставленным суффиксом восстанавливают ровно библиотечный body: без дублирования уже принятого префикса, без потери и с одним переходом text → document. Текущая идея `chunks[index..].concat()` верна для splitter-контракта, но тест должен доказывать именно end-to-end инвариант.
4. **Огромный вход признан риском, но оставлен без границы.** `fs::read` и `parse` целиком могут исчерпать память процесса; `spawn_blocking` защищает async runtime от блокировки, но не от OOM всего hub. Нужна небольшая защитная граница в hub (без изменения pure crate): до чтения проверить metadata и читать не более `MAX_TRANSCRIPT_BYTES + 1`; oversized файл даёт понятный notice. Тест использует sparse file, а не реально выделяет десятки мегабайт.
5. **«Polling продолжает работать» доказано только конструктивным рассуждением.** Добавить async-тест: подготовка команды искусственно задержана/заблокирована в command worker, а fake `UpdateSource` продолжает выдавать следующий batch и offset сохраняется. Это подтверждает, что callback poll только ставит `Inbound` в очередь и не выполняет IO/parse.
6. **Шаг «скопировать 13 файлов байт-в-байт» больше неприемлем.** Reference полезен как прототип, но минимум `commands.rs`, `updates.rs` и их тесты должны отличаться по пунктам выше; старые хэши нельзя использовать как критерий готовности.

### Что в исходном плане подтверждено

- Узкий `TranscriptLocator::locate(thread_id, session_prefix)` пригоден для замены на slot → current session в TASK-011.
- Нерекурсивный resolver исключает `subagents/*.jsonl`; UUID shape, mtime и ambiguity покрыты разумно.
- `last_prompts` переиспользует тот же `is_prompt`, что и `tool_after`; diff меняет существующий renderer только механическим выделением предиката. Существующие fixture expectations обязаны остаться неизменными.
- Последовательный command worker и ожидание каждого scheduler delivery сохраняют порядок чанков; file IO/parse находятся в `spawn_blocking`.
- Однократный 400 `message is too long` → document без обратного retry соответствует требованию и Bot API модели ошибок.
- `OffsetStore` пишет temp в том же каталоге, `sync_all` и rename поверх старого файла; тесты round-trip и оставленного temp корректны для требуемой old-or-new семантики на поддерживаемой файловой системе. `std::fs::rename` заменяет destination, но платформенные различия остаются риском: [Rust std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html).
- Privacy-разделение корректно: routed inputs не несут Telegram user id; ошибки чтения не выводят путь; reqwest errors уже очищаются через `without_url()`; существующие fixture/privacy tests дополняются отдельным `command_logs` binary.

## 2. Updated understanding

- Сейчас `hub::run` загружает безопасный config, проверяет бота и права, запускает единый `Scheduler`, затем `updates::poll`; входящие сообщения пока только логируются. `updates::poll` держит offset лишь в памяти и принимает конкретный `BotApi`.
- `Scheduler` уже является единственным outbound actor. `Op::Send` и `Op::SendDocument` находятся в одной message lane, `Outbox::submit` возвращает completion receiver, поэтому command delivery может строго ждать каждый чанк.
- `transcript` остаётся pure crate с `parse`, `render_brief`, `render_full`, `split_for_telegram`; IO должен быть только в hub. Для `[n]` нужен public pure `last_prompts(&[Turn], n) -> &[Turn]`, использующий существующую классификацию prompt.
- До TASK-011 реального slot registry нет. Принятое временное правило: искать только `<projects_root>/<project>/<uuid>.jsonl`; без prefix выбирать максимальный mtime, с prefix возвращать ровно одно совпадение или ambiguity list. `thread_id` уже входит в seam, но `ProjectsDir` временно его игнорирует.
- Решения оркестратора окончательны: defaults brief=3/full=1, range 1..=100; `CLAUDE_CONFIG_DIR` не автоопределяется, используется `CCTG_PROJECTS_DIR`; live Telegram round-trip/RSS не блокирует merge; в этой задаче субагент показывается только строкой `↳`, bodies остаются TASK-015.
- At-most-once относится к Telegram update batch, не к гарантии ответа: durable offset сохраняется перед dispatch. Краш после save может потерять весь batch или его ещё не поставленный/необработанный хвост; повторного автоматического ответа зато нет.

## 3. Revised approach

1. Добавить в `transcript` только `last_prompts`, вынеся существующий prompt predicate без изменения поведения `render_brief`/`render_full`.
2. Добавить `hub/sessions.rs` с `TranscriptLocator` и `ProjectsDir`. Сканировать ровно два уровня через `read_dir`, принимать только direct regular files с lowercase UUID stem, сортировать по mtime desc, затем по session id и project name для детерминизма. Prefix никогда не подставлять в path. Ambiguous результат не угадывать.
3. Добавить `hub/commands.rs`: parse → blocking prepare → ordered delivery. `Reply.body` — ровно library rendering, без header. Short session id используется для имени/caption документа и безопасного лога; project name допускается только в user-facing caption/ambiguity notice и никогда не логируется. Missing/unreadable/oversized/empty render превращаются в notice, worker продолжает цикл.
4. Ограничить входной transcript константой `MAX_TRANSCRIPT_BYTES` (64 MiB для MVP): metadata precheck плюс bounded read `take(MAX + 1)` закрывают sparse/обычный файл и race роста. Это не Telegram output threshold; manageable input всё равно может дать document по `split_for_telegram`.
5. Доставка: `split_for_telegram(body, default options)`. `prefer_file` — один document с полным body. Иначе отправлять chunks строго последовательно. При первой ошибке text send `400` с `message is too long` отправить одним document только недоставленный suffix и завершить; остальные ошибки завершают reply. Никогда не повторять document как text.
6. Command worker остаётся отдельной последовательной задачей. Poll callback только `try_send`/`send` в command queue; тест обязан доказать независимость poll от долгого prepare. Если сохраняется unbounded channel из reference, оставить его явно как MVP trade-off и не утверждать bounded memory.
7. Добавить `hub/offset.rs`. В `updates.rs` ввести `UpdateSource`; load offset один раз. Для batch с новым offset сначала retry `OffsetStore::save(next)` с ограниченным backoff до успеха, затем обновить in-memory offset и dispatch. Так ни один обработанный batch не остаётся без durable marker.
8. Config: `CCTG_PROJECTS_DIR`, default `<USERPROFILE|HOME>/.claude/projects`; `CCTG_STATE_DIR`, default `.cctg`. Не читать `CLAUDE_CONFIG_DIR`, `.env` values не логировать и не переносить в process env.
9. Использовать только уже согласованные crates; единственное dependency change — локальный `transcript = { path = "../transcript" }` в `cctg`, соответствующая строка в `Cargo.lock`.

## 4. Revised steps

### Step 0 — baseline и изоляция

1. Проверить `git status --short -- Cargo.toml Cargo.lock crates`; не затирать чужие изменения. Существующий `maw/tasks/in_progress/TASK-009/metrics.md` не относится к product diff.
2. Не читать `.env` и реальные значения секретов; не вызывать Telegram API.
3. Назначить отдельные target dirs: `%TEMP%/cctg-task009-baseline-target` для HEAD и `%TEMP%/cctg-task009-impl-target` для implementation/reference. Не делить один target между workspace-копиями.
4. Последовательно выполнить baseline `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, `cargo test --workspace --offline`; ожидать 110 passed + 1 ignored на исходном HEAD.

### Step 1 — pure transcript slice

1. В `crates/transcript/src/render.rs` вынести private `is_prompt` из `tool_after` и добавить `pub fn last_prompts` с семантикой: n-й prompt с конца, меньше n → весь slice, n=0 → empty.
2. Re-export из `crates/transcript/src/lib.rs`; не добавлять IO или dependency.
3. В `crates/transcript/tests/render.rs` добавить:
   - последние 1/2/99 exchanges;
   - service/meta/tool_result не считаются boundary, `<channel>` и slash command считаются;
   - существующие `FINAL_ANSWER_BRIEF/FULL` и все прежние render tests остаются byte-for-byte теми же.

### Step 2 — resolver и config

1. Создать `hub/sessions.rs` с `Located`, typed `LocateError`, `TranscriptLocator`, `ProjectsDir`.
2. Покрыть: newest mtime across projects; deterministic tie; unique/full/ambiguous/no-match prefix; missing root; unreadable root; junk/root-level files/directories; symlink skipped (тест только там, где ОС разрешает создать ссылку без повышенных прав); nested subagent UUID and `agent-*` never selected.
3. Расширить `hub/config.rs` полями `projects_dir`/`state_dir` и тестами defaults/overrides. Принять решения оркестратора: no `CLAUDE_CONFIG_DIR` autodetect; brief/full defaults 3/1, range 1..=100.

### Step 3 — durable offset

1. Создать `hub/offset.rs`: create state dir, load decimal offset, missing → None, corrupt/unreadable → fixed warning without path/value; save через same-dir `offset.tmp`, write, `sync_all`, close, rename.
2. В `hub/updates.rs` добавить `UpdateSource` и изменить `poll`: route candidate batch, durable-save candidate next offset, только после успеха commit in-memory offset и call handlers.
3. При save error не dispatch-ить batch; retry save с backoff. Не refetch и не duplicate уже удерживаемый batch.
4. Тесты:
   - round-trip/replacement, corrupt file, stale temp;
   - simulated restart: update 5 обработан один раз, persisted 6 отсекает его, update 6 проходит;
   - искусственная save failure: handler остаётся не вызван, после устранения failure тот же batch сохраняется и dispatch-ится ровно один раз;
   - crash-window semantics документирована: после успешного save и до handler update может быть потерян, но не replayed.

### Step 4 — commands и безопасное чтение

1. Создать `hub/commands.rs` с parsing `/brief|/full[@bot] [n] [prefix]`, case-insensitive command/bot name, lowercase prefix validation, usage для invalid range/shape.
2. В `prepare` вызвать resolver, открыть файл, проверить metadata length, читать bounded до `MAX_TRANSCRIPT_BYTES + 1`, затем `from_utf8_lossy` → `parse` → `last_prompts` → выбранный renderer. Oversized/missing/unreadable/empty дают русские notices без path/project.
3. `Reply.body` не содержит header. Для text delivery chunks должны быть ровно `split_for_telegram(renderer_output).chunks`; для prefer-file bytes ровно renderer output. Document caption ≤1024 и filename используют view + short id, не private project path.
4. Реализовать 400-too-long fallback один раз. End-to-end тест собирает accepted text prefix + document suffix и сравнивает с exact renderer output; отдельно проверить отказ самого document — после него нет новых операций.
5. Добавить sequential `serve` и `spawn_blocking`; не позволять panic/join error остановить worker.
6. Тесты команд:
   - все семь существующих anonymized fixtures × brief/full/prefix: output без wrapper равен библиотеке;
   - multi-chunk order и concat exact body;
   - >4 chunks → один document exact body;
   - 400 too long → один suffix document, no loop/no duplicate;
   - other 400 → no document;
   - missing, unreadable, oversized sparse file, empty render, no match, ambiguity, usage; после каждого следующий valid command исполняется.

### Step 5 — wiring и poll independence

1. В `hub/mod.rs` зарегистрировать новые modules, открыть `OffsetStore`, запустить scheduler и один command worker, передать `ProjectsDir`, username и inputs из poll callback. Не менять routing остальных inputs.
2. Добавить async-тест с gated/slow locator или prepare: пока command worker занят, fake poll принимает следующий batch и durable offset продвигается; после снятия gate команды отвечают по порядку. Тест не создаёт реально огромный heap object.
3. Субагенты в TASK-009 остаются только строками `↳` через обычные renderers; не читать `.meta.json`/subagent files здесь.

### Step 6 — privacy, dependencies и полная проверка

1. Добавить отдельный `crates/cctg/tests/command_logs.rs` с `.without_time()`: marker в project/path не встречается ни в logs, ни в notices; short synthetic session id допустим. Существующие `transport_errors_never_contain_the_token`, `routing_logs_never_contain_user_ids`, `fixtures_have_no_private_data` остаются обязательными.
2. Проверить product diff на `Users[\\/-]user`, `AppData`, реальные абсолютные пути и token-like literals, не читая `.env`. Synthetic ids/paths должны быть явно фиктивными.
3. Последовательно, с implementation target вне repo: fmt, clippy `-D warnings`, full workspace tests offline. Затем targeted повторы command/offset/log tests для флейков.
4. Проверить `git status`: нет `target/`, `.cctg/`, `.env`, scratch или реальных fixtures в product change.
5. `IMPL_SUMMARY.md`: перечислить файлы, результаты проверок, at-most-once loss window и то, что live `/brief`/RSS smoke по решению оркестратора не блокирует merge. Реальный Telegram не вызывать.

### Acceptance criteria → tests

| Критерий | Обязательное доказательство |
|---|---|
| exact library output и chunk order | fixture matrix сравнивает sent chunks/document bytes непосредственно с `render_*(last_prompts(...))`, без header; multi-chunk concat exact |
| large output/document и 400 fallback | prefer-file exact body; accepted prefix + document suffix exact body; document attempted once; other 400 no fallback |
| restart без replay | persisted-offset restart test плюс save-failure-no-dispatch test |
| bad path и polling жив | notice sequence test плюс slow/oversized command не блокирует fake poll |
| privacy | isolated command log test + существующие token/user-id/fixture privacy tests + diff scan |
| real source resolver | newest mtime, deterministic tie, exact/ambiguous prefix, missing/unreadable root, narrow trait |
| no subagent as session | nested `agent-*` и UUID файлы, directory case; symlink поведение подтверждено std docs и capability-gated test |
| existing tests | full offline workspace suite green |

## 5. Risk areas

- **At-most-once теряет данные.** После durable save crash может потерять весь batch; crash во время последовательного dispatch — его необработанный хвост. Это сознательный выбор против duplicate Telegram replies. Пользователь повторяет команду вручную.
- **Persistent state unavailable.** Новый план намеренно приостанавливает dispatch/poll progress на retry сохранения offset, а не отвечает без durable marker. Нужен заметный warning без path/value; восстановление диска продолжает тот же batch.
- **Windows/filesystem rename semantics.** Проверяется replacement round-trip на целевой платформе; std описывает platform differences и не обещает crash durability parent directory. Corrupt/missing final offset приводит к warning и возможному replay после внешнего повреждения.
- **Transcript size boundary.** 64 MiB — MVP safety cap, а не форматное ограничение Claude. Oversized session получает notice; полноценный tail reader остаётся TASK-016. Race роста закрывается bounded read.
- **Concurrent writer.** Последняя JSONL строка может быть incomplete/invalid UTF-8; `from_utf8_lossy` и line-tolerant parser сохраняют предыдущие records. File disappearance после locate превращается в notice.
- **Resolver до TASK-011.** Global newest-by-mtime может выбрать другую активную local session; explicit prefix устраняет ambiguity. Это принятый временный источник, заменяемый slot registry через тот же trait.
- **Command queue.** Один worker сохраняет порядок, но медленная команда задерживает следующие. Если остаётся unbounded channel, allowlist снижает threat, но не даёт bounded memory; не скрывать этот trade-off. Bounded queue — отдельное решение, если нагрузка станет реальной.
- **Telegram limits and error strings.** Text считается через library UTF-16-conservative splitter; document ≤50 MB, caption ≤1024. Fallback распознаёт только Telegram 400 с size-specific description и выполняется один раз.
- **Privacy in Telegram vs logs.** Ambiguity notices могут показывать encoded project names пользователю закрытой allowlisted-группы, но они никогда не попадают в logs. Если это UX/privacy требование изменится, заменять display label отдельно от resolver path.
- **Live smoke.** По решению оркестратора реальный `/brief` round-trip и RSS long-poll не блокируют merge; после merge orchestrator делает безопасную hub-only проверку, пользователь — Telegram round-trip.
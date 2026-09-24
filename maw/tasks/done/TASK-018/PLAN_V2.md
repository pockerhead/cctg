# TASK-018 PLAN V2: надёжный hook spool и multi-slot soak

## 1. Review notes

### Проверенный контрпример (disconfirmation first)

До оценки плана был проверен сценарий, в котором hook и agent одной сессии одновременно переигрывают один spool-файл, один POST завершается успешно, а второй конкурирует с удалением файла. Узкий контрпример **не подтвердился**: `spool::replay` удаляет файл только после успешного POST и игнорирует конкурирующий `NotFound`, а `hub::ingress::accept_hook` проверяет и записывает `event_id` под одним mutex, поэтому настоящий `serve_hooks` передаёт событие actor-у один раз.

Проверка выявила более широкий реальный race: `scratch/planner/ws/crates/cctg/src/spool.rs:140-161` считает файлы и затем пишет новый файл без межпроцессной синхронизации. Два hook-процесса могут одновременно увидеть свободное место и превысить `MAX_PER_SESSION` или `MAX_FILES`. Прямого теста конкурентного replay/save в референсе нет. Результат проверки сохранён в `scratch/reviewer1_disconfirmation.md`.

### Конкретные недостатки исходного плана и референса

1. **Решения оркестратора не включены.** Раздел Open questions всё ещё оставляет Q1–Q3 открытыми, хотя `OPEN_DECISIONS.md` уже решил их: Stop-тексты не сохранять; чужие SessionEnd не переигрывать; live-soak обязан удалить созданные темы. Также отсутствует обязательное исправление утечки `%TEMP%/cctg-test-*-slots`.

2. **Spool не гарантирует заявленные bounds при конкуренции.** `save()` выполняет `prune/count/write` без lock. Кроме того, `files()` учитывает только корректно названные `.json`; оставшийся после падения `.tmp` никогда не стареет, не очищается и не входит в общий лимит. Тест `the_spool_is_bounded...` последовательный и race не ловит.

3. **Запись атомарна только с точки зрения видимости, но не crash-durable.** Референс использует `std::fs::write` и `rename`, не вызывает `File::sync_all`. В самом проекте `RegistryStore::save` уже применяет `write_all + sync_all + rename`. Документация Rust отдельно предупреждает, что обычное закрытие файла не обязано сбрасывать данные на носитель: [std::fs::File::sync_all](https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all). Для дискового recovery-spool следует повторить проверенный проектный шаблон.

4. **Проверка скорости hook неполная.** Тест `a_failed_kept_event_stops_the_hook_within_its_budget` измеряет только `deliver()`. После исчерпания общего сетевого deadline реальный `hook::run` ещё синхронно сканирует spool и пишет файл. Поэтому утверждение «down hub costs exactly what it cost before» не доказано. Нужен wall-clock тест настоящего `cctg hook`, включая сохранение при полном/загрязнённом spool.

5. **Agent может породить несколько параллельных replay-задач.** `spawn_replay` запускается после каждой регистрации и не хранит handle. При быстром reconnect предыдущий 5-секундный replay ещё может работать. Дедуп защищает hub, но лишняя конкуренция усугубляет race, нагрузку и порядок. Replay одного agent должен быть single-flight.

6. **Live mode небезопасен для реальной группы.** В `soak.rs:727-744` все реальные `forum_topic_edited` из общей очереди `getUpdates` передаются в тестовый hub. Это может удалить служебное сообщение чужой темы. Более того, отдельный poller подтверждает общую очередь обновлений бота и способен лишить штатный hub пользовательских апдейтов. Синтетические `React`/`AnswerCallback` также отправляются в реальный чат по выдуманным id. Это противоречит требованию «только свои темы».

7. **Live cleanup отсутствует.** `docs/soak.md` предлагает ручное удаление, а `soak.rs` вообще не вызывает `deleteForumTopic`. Это прямо противоречит решению оркестратора. Официальный Bot API поддерживает `deleteForumTopic` и требует `can_delete_messages`: [Telegram Bot API](https://core.telegram.org/bots/api#deleteforumtopic).

8. **Cleanup не переживает panic.** `Hub::stop` вызывается только в успешном хвосте сценария; у `Hub` нет `Drop`. При assertion failure задачи poller/scheduler остаются detached до остановки runtime, а временный каталог и live-темы не очищаются. Сценарий нужно запускать в отдельной Tokio-задаче, всегда останавливать hub и выполнять cleanup до повторного panic.

9. **`registry.json` проверен не «field by field».** Проверяются основные поля slot, но у session пропущены как минимум `transcript_path`, `title`, `seen`, `stream` и содержимое nested `block`; не проверяется точный набор сериализованных ключей. Тест может пропустить неверный transcript или лишнее устойчивое состояние.

10. **Проверка permission priority полезна, но доказательство нужно сделать явным.** Сейчас `behind_a2 > 0` показывает строки после permission, но комментарий сильнее факта: он утверждает, что эти строки уже стояли в scheduler до prompt. Перед отправкой permission следует зафиксировать наблюдаемое состояние backlog (исчерпанный fake bucket/активная 429-пауза и число уже переданных/оставшихся строк), а отчёт должен различать enqueue-time, send-time и latency.

11. **Утечка slot temp dirs подтверждается кодом.** `Slots::new` (`slots.rs:409-410`) detach-ит `save_loop`; `Slots::run` бесконечен даже после закрытия всех трёх входов; `Rig` теряет join handle, а `TempDir::drop` молча удаляет каталог. `spawn_blocking(RegistryStore::save)` может держать файл открытым или закончить после drop. Поэтому остаётся каталог с поздно записанным `registry.json`. Исправлять надо lifecycle actor/saver, а не маскировать утечку повторными `remove_dir_all`.

12. **Размер soak.** Около 1600 строк нельзя радикально уменьшить без потери существенного покрытия: большая часть — stand-in процесса, fake Telegram, real ingress/agent protocol и проверки сценария. Однако 560-строчный `scenario` надо разбить на пять фаз и финальный validator; это уменьшит когнитивную нагрузку, хотя общий объём сократится лишь умеренно. Не следует выносить общие test abstractions в production crate ради одной задачи.

13. **Research verification.** Официальный Telegram FAQ по-прежнему указывает предел 20 сообщений/мин для group и рекомендует не превышать примерно 1 сообщение/с в одном chat; `retry_after` остаётся официальным указанием времени до повторной попытки: [Bots FAQ](https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this), [ResponseParameters](https://core.telegram.org/bots/api#responseparameters). Поэтому текущая архитектура одного bucket и глобальной паузы на 429 верна; edits/topic mutations следует считать отдельно, а не приписывать им неподтверждённый числовой лимит.

## 2. Updated understanding

- Текущий HEAD проекта — `b05706f`; продуктовый код совпадает с базой `114e786`, а последующие изменения — только task/plan artifacts. `scratch/planner/task018.patch` проходит `git apply --check`, но не включает поздние решения из `OPEN_DECISIONS.md` и потому не должен применяться вслепую.
- Без spool поздний SessionStart действительно теряется: hook делает один POST, unknown agent остаётся в `Slots::pending`, а неизвестные Stop/UserPromptSubmit registry игнорирует. Premise challenge корректен.
- Hub уже умеет безопасно принять поздний SessionStart: top-level сессия занимает первый свободный slot, `occupy` создаёт один pending separator, а ingress дедуплицирует повторный `event_id` после успешного handoff.
- В spool нужны только `SessionStart` и `SessionEnd`. Это сохраняет необходимые lifecycle/pid/path данные, но исключает prompt и assistant text. По решению оркестратора ответы, потерянные во время down hub, не восстанавливаются; device-wide replay чужих SessionEnd остаётся post-MVP ограничением.
- Spool — локальная очередь устройства: абсолютный `CCTG_STATE_DIR` имеет приоритет, иначе используется стабильный `<home>/.cctg`; относительное значение не должно приводить к записи в cwd сессии. Hook и agent должны вычислять один и тот же путь и иметь отдельные тесты process-env/device.env/default/no-home.
- Реальный nesting определяется только `cctg hook` через process-tree. Stand-in `claude.exe`, запущенный через быстро завершающийся launcher, воспроизводит top-level; nested stand-in как ребёнок A1 воспроизводит родительский `claude.exe`. Это лучше прямой инъекции HookPost.
- Fake soak уже хорошо покрывает real `serve_hooks`, `serve_agents`, `Slots`, `Scheduler`, `updates::poll`, настоящие процессы `cctg hook`/`agent`, три темы, slot reuse, 429 и permission routing. Его надо укрепить, а не переписывать.
- Live soak не может безопасно читать общую очередь `getUpdates` реального бота: Bot API не фильтрует updates по topic. В live режиме authoritative проверка удаления `forum_topic_edited` невозможна без риска потерять чужие updates; её полностью выполняет fake e2e. Live режим проверяет реальные create/edit/send и затем удаляет только созданные им topic ids.
- Причина `cctg-test-*-slots` — detached saver/actor, а не `TempDir` сам по себе. Успешное завершение каждого actor-backed unit test должно закрыть входы, дождаться `Slots::run`, затем дождаться save loop, и только потом drop-нуть TempDir.

## 3. Revised approach

### A. Bounded, durable, concurrent hook spool

Сохранить maildir-подобную схему `<state>/spool/<session>/<timestamp>-<event_id>.json`, но сделать операции обслуживания/сохранения межпроцессно сериализованными через небольшой lock-файл, создаваемый `create_new`. Lock имеет короткий bounded wait и безопасное восстановление заведомо stale lock; ошибка lock выражается фиксированным `SpoolError`, без пути или содержимого. Под lock выполняются prune, подсчёт и publish нового файла, поэтому лимиты 16/session, 256 total и 16 KiB/file остаются истинными при двух hook-процессах.

Файл писать уникальным sibling `.tmp`: `File::create_new`, `write_all`, `sync_all`, close, `rename` в `.json`. Reader видит только `.json`. Старые `.tmp` после grace удаляются; свежий `.tmp` считается незавершённой записью и не читается. Повреждённый/foreign/oversized `.json` удаляется при scan. Replay остаётся at-least-once; удаление происходит только после 2xx, `event_id` остаётся исходным.

Hook сначала replay-ит события своей сессии, затем собственное, используя один deadline для всех POST. При первом отказе порядок не нарушается. После сетевой ошибки собственный Start/End сохраняется. Wall-clock CLI тест должен доказать, что down hub не умножает timeout на число файлов; небольшая дисковая стоимость измеряется отдельно и остаётся внутри SessionEnd budget на тестовом host.

Agent после успешной регистрации запускает один background replay своей сессии. Новый reconnect отменяет/дожидается прежней replay-задачи, так что на один agent приходится не более одного replay. Hook/agent всё ещё могут конкурировать друг с другом — это проверяется настоящим `serve_hooks` и является безопасным благодаря hub dedup.

### B. Lifecycle-safe slots tests

`Slots` должен владеть join handle save loop. `Slots::run` отслеживает закрытие agents/hooks/control; когда все внешние входы закрыты, он прекращает tick loop, закрывает sender snapshot-ов и ждёт save loop, включая последний `spawn_blocking` save. Unit-test `Rig` хранит actor handle и предоставляет обязательный `shutdown(self).await`: сначала закрывает senders, затем ждёт actor/saver, затем позволяет drop TempDir. Все actor-backed slots tests завершаются через этот путь. После набора тестов отдельная проверка сравнивает множество `%TEMP%/cctg-test-*-slots*` до/после и требует отсутствия новых каталогов.

### C. Fake soak as the acceptance authority

Сохранить real-process stand-in и fake transport. Разбить сценарий на `phase_start_and_route`, `phase_nested`, `phase_burst_and_permissions`, `phase_hub_down_recovery`, `validate_and_report`. Fake transport продолжает генерировать service update на каждую принятую topic edit и два детерминированных 429.

До permission запросов зафиксировать backlog marker: burst уже прочитан одним transcript chunk, fake bucket исчерпан либо действует 429-pause, и остаются строки A2. Затем измерить от момента записи permission RPC до принятого `sendMessage`; доказать, что permission отправлен до ранее зафиксированного хвоста stream. Для 429 проверять глобальную тишину не менее `retry_after`, следующий вызов — тот же op, ровно один успешный retry; отдельный существующий scheduler unit test остаётся источником покрытия повторных 429.

Финальный registry validator проверяет точный набор persistent ключей и значения всех полей: три slot, пять session, nested block/parent/slot, transcript paths, pid, title, ended, stream state, pending separator, buffer, `pids`, отсутствие неожиданных subagent records. Динамические `seq/seen/offset/message_id` проверяются через строгие инварианты, а не игнорируются.

### D. Safe live mode

Live mode запускается только вручную при остановленном штатном hub, но **не вызывает real `getUpdates` вообще**. Тем самым он не подтверждает и не теряет чужие updates. Synthetic inbound/callback по-прежнему подаются прямо в `Control`, однако live transport локально подтверждает `React` и `AnswerCallback`, не отправляя операции с выдуманными ids в Telegram. Все реальные send/edit/delete-message операции допускаются только для topic/message ids, созданных текущим soak.

Перед стартом проверить права bot на create/edit и delete topics. Записывать каждый успешно созданный topic id. Сценарий выполнять в отдельной Tokio task; `Hub` получает `Drop`, который abort-ит принадлежащие ему задачи. После normal result или JoinError выполнить прямые, test-only HTTPS вызовы `deleteForumTopic` для этих id в обратном порядке, с sanitised errors и соблюдением `retry_after`; токен никогда не попадает в Debug/report. Только после cleanup вернуть отчёт или повторно завершить тест ошибкой. Cleanup failure — failure live soak с фиксированным сообщением и списком только topic ids, без token.

## 4. Revised steps

1. **Зафиксировать baseline и не применять reference patch целиком.**
   - Проверить `git apply --check scratch/planner/task018.patch` и hashes, но переносить изменения с поправками ниже.
   - Снять список существующих `%TEMP%/cctg-test-*-slots*` только для сравнения; не удалять чужие/старые каталоги в реализации теста.

2. **`crates/cctg/src/device.rs`: общий стабильный state path.**
   - Добавить `DeviceConfig.state_dir` и `DEVICE_STATE`.
   - Absolute `CCTG_STATE_DIR` брать из process env с обычным приоритетом над `device.env`; иначе `<home>/.cctg`; относительное значение никогда не использовать относительно session cwd.
   - Тесты: process env, device.env, default, relative value, missing home; e2e дополнительно проверяет, что spool появился только в ожидаемом каталоге.

3. **Новый `crates/cctg/src/spool.rs`: durable queue.**
   - Оставить только Start/End; валидировать session id `[A-Za-z0-9_-]{1,128}`.
   - Ввести fixed-text ошибки `NotKept`, `BadSession`, `Busy`, `Full`, `TooLarge`, `Io(ErrorKind)`.
   - Реализовать глобальный межпроцессный lock для `prune + count + publish`; ожидание lock ограничить малой частью hook budget, stale lock распознавать только после безопасного возраста.
   - Писать unique `.tmp` через `create_new`, `write_all`, `sync_all`, затем rename в том же каталоге. Не сериализовать secret; не сохранять Stop/UserPromptSubmit/Subagent payloads.
   - `scan/pending`: стабильный oldest-first порядок; удалить expired, corrupt, foreign, oversized `.json`; свежий `.tmp` не читать, stale `.tmp` удалить; пустые session dirs убирать.
   - `replay`: один deadline, stop-on-first-error, delete-after-success, racing `NotFound` считать успехом удаления.
   - Unit tests: kinds/privacy, exact ordering/event ids, file/session/total/age bounds, clock edge, invalid ids, half-written fresh/stale temp, corrupt/foreign file, fs publish failure, concurrent saves at both caps, two concurrent replays through real `serve_hooks`, failure preserves tail, duplicate accepted once.

4. **`crates/cctg/src/hook.rs`: replay-before-own под общим deadline.**
   - Вычислить spool из `DeviceConfig`; replay только той же session.
   - Все POST используют остаток одного timeout; после первого failure собственное событие не обгоняет сохранённое.
   - На failure сохранять только Start/End и писать только fixed stderr. Hook всегда exit 0 и stdout empty.
   - Добавить real CLI wall-clock tests: silent hub + 0/1/16 pending files; общий runtime не содержит N сетевых timeout и остаётся в принятом бюджете; 503 оставляет own event позади; недоступный/занятый spool не печатает id/path/body/secret.

5. **`crates/cctg/src/agent.rs`: single-flight replay после register.**
   - Расширить `LinkConfig` replay-настройками без wire-version change.
   - После подтверждённой регистрации запускать background replay session spool в hook endpoint с 5 s budget.
   - Хранить handle/generation; новый reconnect не оставляет предыдущий replay параллельно. Завершение agent отменяет replay.
   - Логи содержат только count/fixed error kind.

6. **`crates/cctg/tests/spool_e2e.rs`: acceptance path настоящими процессами.**
   - Down hub на SessionStart → ровно один валидный файл; следующий hook доставляет byte-equivalent Start перед собственным событием; повтор того же файла hub дропает.
   - Agent reconnect без следующего hook сам доставляет Start и освобождает spool.
   - Одновременно запустить hook replay и agent replay одной session; actor получает event один раз, spool пуст.
   - Два concurrent lifecycle hooks не превышают 16/session и 256 total; все опубликованные JSON целы.
   - Stop/prompt text и secret отсутствуют во всех файлах и stderr; проверять exact JSON keys/event variants, а не поиск одного слова.

7. **`crates/cctg/src/hub/slots.rs` и его tests: устранить leak saver.**
   - Save-loop join handle принадлежит `Slots`; при закрытии всех внешних input channels `run` завершает pump, закрывает watch sender и await-ит saver.
   - Unit `Rig` хранит actor handle и заканчивает каждый тест через `shutdown(self).await` до drop TempDir. Прямые tests с `Slots` используют тот же shutdown primitive.
   - Regression test создаёт dirty registry, shutdown-ит actor, проверяет final `registry.json`, drop-ит temp dir и убеждается, что путь не появился снова. Suite-level probe подтверждает отсутствие новых `cctg-test-*-slots*`.

8. **`crates/cctg/Cargo.toml` и структура soak.**
   - Оставить `harness = false`, skip без `--ignored`.
   - Сохранить один integration target; разбить крупный scenario на фазовые функции (при необходимости private modules рядом с `soak.rs`, без production abstractions).
   - Stand-in по-прежнему копирует текущий test exe под именем `claude(.exe)`, launcher завершается до hooks, nested остаётся ребёнком A1. Assert-ить hook pid и agent register pid, чтобы fidelity не стала декоративной.

9. **Fake scenario и строгие assertions.**
   - A1/A2/B1 → ровно темы A/A#2/B; двусторонний routing, reply, transcript stream и Stop answer только в своей теме.
   - Nested через реальный process-tree walk → ни одной новой темы, parent=A1 и block только в A.
   - Burst A2/B1 → FIFO, fake bucket/min_gap, два 429 с полной pause/retry проверкой, два permission prompt и правильные verdict agents; latency и backlog marker в report.
   - A1 end; hub stop; A5 Start сохраняется; agent-triggered replay проверен отдельно, а в scenario hook/agent race допустим; после restart A5 занимает slot 0, появляется ровно один separator, topic count остаётся 3.
   - Fake `forum_topic_edited` проходит через настоящий `updates::poll` и каждый его message id имеет успешный `deleteMessage`; left=0.
   - Report раздельно считает accepted/error/429 для send, permission, stream, document, edit, react, callback, create-topic, edit-topic, delete-message. Только metered message operations сравниваются с bucket; edits/topic calls — отдельные счётчики.

10. **Полный validator `registry.json`.**
    - Проверить version/seq, exact key sets верхнего уровня, каждого Slot и SessionEntry.
    - Slot: host, canonical folder key/name, ordinal, topic/current session, applied title/icon, null pending separator, idle/omitted buffer.
    - Sessions A1/A2/B1/A5: host, top-level kind, slot, expected transcript path, stand-in pid, title, ended, seen, complete stream invariants.
    - Nested: host, parent A1, slot A, transcript path, pid/end, exact block header/result/state; no own topic.
    - `pids` ровно A2/B1/A5; `subagents` пуст; никакого лишнего durable state.

11. **Live-mode safety и cleanup.**
    - Не запускать `updates::poll` с real `BotApi`; fake mode остаётся проверкой service-update deletion.
    - Live transport отправляет в Telegram только операции, чьи thread/message ids принадлежат темам текущего soak; synthetic reaction/callback подтверждает локально и помечает в report как synthetic.
    - Preflight прав, capture трёх returned topic ids, panic-safe cleanup через прямой `deleteForumTopic` (без нового production `BotApi` method), retry_after и fixed/redacted errors.
    - `Hub: Drop` abort-ит все свои tasks. После scenario success/failure закрыть stand-ins, cleanup topics, удалить temp tree; затем report/ошибка.
    - Live test не запускается агентом автоматически и не вызывается при обычном workspace test.

12. **`docs/soak.md`: документация без нерешённых вопросов.**
    - Описать fake как обязательный acceptance run, live как ручной smoke run.
    - Явно сказать: real updates не читаются; чужие темы/сообщения не изменяются; три созданные темы автоматически удаляются даже после failure; Stop-тексты down-периода и чужие ended sessions не replay-ятся.
    - Удалить инструкцию ручной очистки и ожидание synthetic 400 errors.

13. **Verification (один cargo за раз, один temp target).**
    - Один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`; в конце обязательно удалить target dir.
    - Итерации: `cargo test -j 1 --offline -p cctg --lib -- spool hook:: device:: hub::slots::`; затем `--test spool_e2e`; затем `--test hook_cli`; затем `--test soak -- --ignored` минимум три раза.
    - До/после slots tests проверить отсутствие новых `cctg-test-*-slots*`.
    - `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; один финальный `cargo test --workspace --no-fail-fast -j 1`.
    - Пересобрать patch/hashes/evidence только после всех поправок. Реальный Telegram API в implementation/review не вызывать.

### Acceptance mapping

| Критерий | Конкретная проверка |
|---|---|
| 5 запусков → ровно 3 темы, routing не смешан | Steps 8–10: topic count, owner map, per-agent inbound, exact registry |
| A5 после down hub reuse slot, 1 separator, 0 новых тем | Steps 4–6 и phase recovery в step 9 |
| Burst, scheduler policy, 429/retry_after без storm | Step 9 + существующие scheduler repeated-429 unit tests |
| Permission впереди stream, latency измерена | Step 9: backlog marker, ordering assertion, two latency values |
| Полный итоговый registry | Step 10 |
| Send/edit/topic-create раздельно | Step 9 report counters |
| Нет оставшихся `forum_topic_edited` | Fake e2e step 9; live topics полностью удаляются step 11 |
| Missed SessionStart replay, bounded/private/idempotent spool через real hook endpoint | Steps 3–6 |
| Existing tests pass | Step 13 |
| Решение по leaked slots dirs | Step 7 + before/after probe in step 13 |

## 5. Risk areas

- **At-least-once ambiguity.** Если hub принял POST, но ответ потерялся, файл будет отправлен снова. В пределах жизни hub это гасит `event_id`; после restart Start должен оставаться idempotent. Это не exactly-once протокол.
- **Ended while hub is down.** SessionEnd сохранится, но после смерти session может не быть следующего hook/agent. По решению оркестратора device-wide replay вне scope; stale slot остаётся известным ограничением.
- **Lock recovery.** Слишком короткий stale threshold может украсть живой lock, слишком длинный задержит recovery после crash. Тестировать реальную конкуренцию и stale-owner case; lock wait не должен съесть hook budget.
- **Disk latency and failure.** `sync_all` повышает надёжность, но может быть медленным. Wall-clock CLI test ловит умножение timeout; hook всё равно exit 0 и при невозможности spool теряет событие с fixed warning, как до задачи.
- **Clock/order.** Age по wall clock чувствителен к переводу часов. Использовать saturating comparisons; не удалять файл с timestamp из будущего как «старый». Filename tie должен иметь детерминированный event-id suffix.
- **Concurrent replay.** Hook и agent могут отправить одно событие; hub dedup обязателен. Agent-side single-flight уменьшает, но не устраняет межпроцессную гонку.
- **Saver shutdown.** Нельзя abort-ить actor и сразу удалять TempDir: финальный `spawn_blocking` save должен быть awaited. Regression проверяет, что каталог не воскресает.
- **Stand-in fidelity.** Launcher/process age/image-name assumptions платформозависимы. Проверки pid/parent/kind должны падать громко, если ToolHelp lineage больше не соответствует реальному Claude Code процессу.
- **Live Telegram.** Даже безопасный live smoke имеет внешние эффекты в трёх своих темах. Он не читает updates, не трогает чужие ids и всегда пытается удалить только captured topic ids. Cleanup failure должен оставить понятные ids для ручной помощи, но никогда token/chat user ids.
- **Rate-limit assertions.** Официальные 20/min относятся к сообщениям группы; edits/topic mutations не имеют подтверждённого числового лимита. Любой 429 всё равно глобально приостанавливает queue на `max(retry_after, 1s)`.
- **Harness size.** Разделение на фазы улучшает ревью, но попытка резко сократить код за счёт моков уберёт именно real process tree, real hook CLI и real ingress, ради которых существует TASK-018.

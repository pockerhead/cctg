# TASK-015 IMPL REVIEW (code-reviewer)

## 1. Verdict

NEEDS_WORK: все AC выполнены и проверены тестами, но одна новая карта (`Slots.indexes`) не ограничена, хотя задача требует предел для каждой новой карты. Исправление в одну строку. Остальное minor или nit.

## 0. Disconfirmation (сделано до оценки)

Контрпример, который сделал бы работу неверной: "typed `SubagentStop` агента `--agent`-сессии (или внутреннего агента) с существующими файлами `agent-<id>.jsonl`/`.meta.json` становится блоком в теме".

Где искал: `slots.rs:640-681` (`on_subagent`), `slots.rs:716-778` (`match_candidates`/`confirm`), `subagents.rs:102-196` (`scan_lines`, `AgentIndex::call`), `registry.rs` `RegistryStore::load` (фильтр legacy-записей), тест `three_explicit_subagents_make_three_blocks_and_internal_agents_none` (`INTERNAL` = `my-agent` с файлами).
Результат: **не подтвердился.** Блок создаётся только через `Registry::confirm_subagent`, а его вызывает только `confirm`, который берёт вызов из `AgentIndex::call`. Для этого нужен `tool_use` с именем `Agent` плюс `tool_result` с тем же `toolUseResult.agentId`. У главного агента `--agent`-сессии такой пары нет. Legacy-записи TASK-011 без `block.header` удаляются при `load` (`a_legacy_subagent_record_never_becomes_a_block` проходит).

Второй проверенный контрпример: "SubagentStop или nested Stop уходит в тему как ответ хода TASK-022". Не подтвердился. `on_turn_answer` вызывается только из ветки `HookEvent::Stop` (`slots.rs:578-586`), а для nested его отсекает `current_slot` (top-level only). `SubagentStop` идёт в другую ветку `match`.

## 2. Confirmed correct

Проверки, которые я прогнал сам (один `CARGO_TARGET_DIR` под `%TEMP%`, `-j 1`, каталог удалён):
- `cargo test -p cctg --lib hub::`: 227 passed, 1 ignored (`scratch/crev_hub_tests.out.txt`).
- `cargo test -p transcript --test subagent`: 15 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: чисто. `cargo fmt --all --check`: exit 0.
- Новых крейтов нет: diff не трогает `Cargo.toml`/`Cargo.lock`.

По AC:
- AC1 (три субагента, одна тема, три блока): `three_explicit_...` проверяет одно `createForumTopic`, три send и три edit, после окна ничего не приходит.
- AC2 (нет призраков): см. Disconfirmation. Пустой `agent_type` и невалидный id отсекаются в `on_subagent` (`slots.rs:656`). Субагенты nested и `NestedUnknownParent` отсекаются через `own_slot` (TopLevel + slot).
- AC3 (порядок fallback): `subagents.rs:617-652` покрывает report > законченный brief > last message (отстающий файл) > пусто. Slot-тест делает то же для S1/S2/S3. Устаревшее чтение тела не перетирает новое: `bodies_waiting` с заменой, проверка в `Done::Body` (`slots.rs:1597-1603`), тест `a_late_body_read_never_overwrites_a_newer_one`.
- AC4 (nested): блок создаётся только при `Nested { parent: Some }` и `slot.is_some()` (`registry.rs:748-763`), повторный старт того же run новый блок не даёт. Для `NestedUnknownParent` блока нет. Nested resume top-level id выходит ранним `return` до создания блока (`registry.rs:669-682`).
- AC5 (target_agent): session берётся из `live_agent`, `subagent_of_message` сверяет parent, thread и message_id (`slots.rs:934-943`, `registry.rs` `subagent_of_message`). Id прошёл `is_agent_id`, ключ проходит `is_meta_key`. В инструкциях канала есть пересылка через SendMessage (`channel.rs:60`).
- AC6 (рестарт): `sending=true` пишется до hand-off, после рестарта блок без `message_id` не отправляется повторно (`block_work`, ветка `None if block.sending`). `Slots::new` помечает блоки уже завершённых сессий как `итог не получен`. `send_refused` повторяет send только при 4xx и `is_connect`.
- Актор не ждёт Telegram и не делает файловый IO: `scan`/`read_body` идут через `spawn_blocking` в отдельном task, send и edit через dispatch, файл через `send_messages`.
- В логах только `short(id)` и фиксированный текст, ни текста, ни путей.
- Пределы: `MAX_CANDIDATES`, `MAX_REPORTS`, `MAX_INDEX_ENTRIES` (внутри одного индекса), `MAX_SUBAGENTS`, `MAX_BLOCK_JOBS`, `MAX_BODY_READS`. Но см. Issue 1.

## 3. Issues

### 1. major: `crates/cctg/src/hub/slots.rs:305` и `:1587-1595`, карта `indexes` растёт без предела по числу сессий

`indexes: HashMap<String, AgentIndex>` удаляется только в ветке `Done::Index` (`slots.rs:1594`): после скана, когда у сессии нет кандидатов и она уже не live top-level. Сессия, у которой был хотя бы один субагент, а потом пришёл SessionEnd, новых сканов не получает (кандидатов нет). Её `AgentIndex` (до 1024 вызовов и 1024 связей с полями до 256 единиц) остаётся в памяти на всё время жизни hub. `grep indexes` подтверждает: других мест удаления нет, `end_blocks`, prune и `lose_blocks` эту карту не трогают. На одну сессию это килобайты, но за недели работы карта растёт с числом сессий, а задача прямо требует предел для каждой новой карты.

Как исправить: в `end_blocks` (или рядом с `close_prompts`) для каждой сессии из `ended_sessions` без кандидатов (`candidates.of_session(s).is_empty()`) и без идущего скана (`!indexing.contains(s)`) делать `self.indexes.remove(s)`. Скан, который закончится позже, всё равно удалит индекс через существующую проверку в `Done::Index`. Тест: субагент подтверждён, SessionEnd, затем `indexes` пуст. Если индекс сознательно держат для resume, то нужен предел по числу сессий, например LRU по `MAX_CANDIDATES`.

### 2. minor: `crates/cctg/src/hub/registry.rs` `lose_blocks`, поздний итог nested run теряется

Комментарий к `lose_blocks` говорит: "A later result still wins". Для субагентов это верно (новый stop снова вызывает `read_body`, а `show_block`). Для nested это не так. Если родитель закончился раньше nested run (например, `/clear` родителя, пока `claude -p` из фонового Bash ещё работает), блок становится `итог не получен`, `answer` выбрасывается, а `end_blocks` на настоящем SessionEnd nested пропускает блок, потому что тот уже не `running`. Последующий Stop nested кладёт `answer`, но показать его некому.
Как исправить: либо поправить комментарий и записать это как ограничение, либо в `end_blocks` показывать `nested_text`, когда answer есть, даже если блок уже не running (при условии, что `message_id` есть). Первое проще и честнее.

### 3. minor: субагенты второго уровня (субагент запускает субагента) молча не получают блок

Вызов `Agent` вложенного субагента лежит в `subagents/agent-<parent>.jsonl`, а не в транскрипте сессии. `scan` смотрит только `transcript_path` сессии, поэтому такой кандидат по окну отбрасывается с `debug!`. Призраков это не даёт, но в "Принятых ограничениях" (PLAN_FINAL 4, IMPL_SUMMARY 2) этого нет.
Как исправить: дописать одну строку в ограничения. Код не менять.

### 4. minor: `crates/cctg/src/hub/slots.rs:786-799` `read_body`, заголовок финального блока может отличаться от running

Описание берётся из `indexes`, который после конца сессии удаляется (а после Issue 1 будет удаляться чаще), и после рестарта hub его тоже нет. Если `.meta.json` при этом нет, финальный текст выходит без `: <description>`, хотя running-блок его показывал. Внутри одного сообщения заголовок "прыгает". Сохранённый `block.header` в registry уже есть.
Как исправить: когда meta нет, брать описание из сохранённого заголовка. Проще всего передавать в `BodyInput` сохранённый `header` и использовать его, если `Subagent::header()` без meta беднее. Можно и принять как nit.

## 4. Missing coverage

- Нет теста "nested `claude -p --resume <top-level id>`: ноль nested-блоков и ни одной новой темы". Поведение есть в коде (ранний `return` в `registry.rs:669-682`), но AC4 про nested, а этот путь не защищён регрессией.
- Нет теста на удаление индекса после конца сессии (Issue 1).
- Нет теста "родитель закончился раньше nested run, потом пришли Stop/SessionEnd nested" (Issue 2), который зафиксировал бы выбранное поведение.
- Edit блока, который Telegram навсегда отвергает (`message thread not found`) и который сдаётся после `MAX_BLOCK_ATTEMPTS`, покрыт только на уровне registry (`a_failed_block_waits_for_the_tick_...`). Через актор с фейком не проверен. Низкий приоритет.

## 5. Nits

- `channel.rs:60`: инструкция говорит "running subagent", а по решению orchestrator `target_agent` ставится и на блок законченного субагента (SendMessage его резюмирует). Текст можно не трогать, но расхождение есть.
- `Registry::block_work` при каждом `pump` проходит все `subagents` (до 1024) и все sessions. Сейчас это не узкое место, пусть так остаётся.
- `scan` при транскрипте больше 256 MiB упирается в `MAX_TRANSCRIPT_BYTES` и перестаёт находить новые вызовы: у очень длинной сессии блоков не будет. Это тот же предел, что у `/brief`, стоит одной строки в ограничениях.
- Остаточный риск, который принят решением orchestrator, записываю для QA: при обычном таймауте reqwest на первом send блок становится tombstone. В Telegram тогда навсегда остаётся "в работе…" без возможности edit, и reply на него приходит без `target_agent`.

## Размер кода

Примерно 1900 строк кода и тестов против 2600 в diff, остальное тесты. Мёртвых абстракций я не нашёл. `Candidates::len`/`is_empty` вызываются только из тестов, но `is_empty` нужен clippy в паре с `len`. Каждая новая часть (кандидаты, индекс, reports, очередь тел, счётчик jobs) закрывает конкретный воспроизведённый сценарий из review-1/2.

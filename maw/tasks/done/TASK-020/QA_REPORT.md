# QA_REPORT — TASK-020

## 1. Environment

Прямой запуск, без сервисов: чистая библиотека `crates/transcript`, docker/compose/dev-сервер нет и не нужны. Ветка `fix/brief-slash-commands` (HEAD dd49e3f), дерево чистое. Код не менялся.

Независимый харнесс: `scratch/qa/` (крейт `qa020`, три бинаря: `main` census, `filediff`, `probes`). Он зависит от нового `crates/transcript` и от копии `crates/transcript` из `main`, переименованной в `transcript_old`. Копию после прогона удалил, чтобы не дублировать исходники в репо. Восстановить:

```
cd maw/tasks/in_progress/TASK-020/scratch/qa
git -C ../../../../.. archive main crates/transcript | tar -x -C old
sed -i 's/^name = "transcript"/name = "transcript_old"/' old/crates/transcript/Cargo.toml
# вернуть в Cargo.toml строку: transcript_old = { path = "old/crates/transcript" }
CARGO_TARGET_DIR=<вне репо> cargo run --release --bin qa020 | --bin filediff | --bin probes
```

`CARGO_TARGET_DIR` был в scratchpad сессии вне репо, после прогона удалён. Реальные jsonl читались только на чтение, в вывод шли счётчики и имена команд, без аргументов и контента.

## 2. Test results

Существующий suite:
- `cargo fmt --all -- --check`: OK
- `cargo clippy --workspace --all-targets -- -D warnings`: OK, без предупреждений
- `cargo test --workspace`: 110 passed, 0 failed, 1 ignored

Новые проверки (свои, не авторские скрипты):

1. Census + differential по всем `~/.claude/projects/**/*.jsonl` (632 файла, 182339 строк). Для каждой user-записи рендер brief и full старой (main) и новой версией.
   - не-meta записи с `<command-name>`: 38 name-first + 5 message-first = 43, все 43 теперь видны в brief (раньше 0 из 43). full у всех 43 равен brief, то есть одна строка.
   - имена: `/model` 18, `/login` 7, `/clear` 6, `/compact` 6, `/feedback` 1 (name-first); `/maw-context` 4, `/maw-tasks` 1 (message-first). Ни одна не дала больше одной строки промпта.
   - meta-записи, содержащие теги (2): скрыты и до, и после.
   - изменения brief/full у записей БЕЗ `<command-name>`: 0. Ни один обычный промпт не переклассифицирован.
   - служебные префиксы в brief скрыты и до, и после: `<task-notification>` 777, `<local-command-stdout>` 32, `<bash-input>` 26, `<bash-stdout>` 26, compact summary 14, `<local-command-caveat>` 40 (meta).
2. Файловый diff (multiset строк) старый vs новый рендер целых транскриптов: brief только добавляет строки (43 команды, 32 пустых разделителя, 10 маркеров «в работе…»), ничего не удаляет. full заменяет 43 сырые многострочные записи команд на однострочные, других изменений нет. Паник нет ни на одном файле.
3. Синтетические probes (`scratch/qa/src/bin/probes.rs`):
   - реальный отступ 12 пробелов, пустые args, message-first с многострочными args, пустой строкой, `<b>` и `</command-name>` внутри args: одна строка, имя на месте.
   - `<command-name>/evil</command-name>` внутри args message-first записи: имя берётся верное (`/m`), текст args сохранён.
   - команда в array-content text блоке, ведущие пробелы/перевод строки: распознаётся.
   - meta-запись с тегами: скрыта. Обычный промпт, где `<command-name>` в середине: остаётся обычным промптом.
   - команда → tool call → end_turn: `tool_after` сбрасывается на команде, текст ассистента до команды со `stop_reason: null` показан как финальный, после tool call ответ `done` показан, маркера нет.
   - команда в хвосте после отвеченного промпта: «в работе…». С последующим `<local-command-stdout>`: stdout скрыт в brief, виден в full, маркер остаётся (service не меняет состояние).
   - `<command-message>clear</command-message>` без имени: остаётся скрытым service.
   - незакрытый `<command-args>` (нет `</command-args>`): команда скрыта в brief, в full сырой текст. См. баг 1.

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| `<command-name>/model…<command-args>opus` → `> /model opus`, пустые args → `> /model` в brief и full | unit-тесты; probes с реальным отступом; census 43/43 реальных записей | PASS |
| многострочные и с `<`/`>` args не ломают строку, имя не теряется | probes (перевод строки, пустая строка, `<b>`, `</command-name>`, `<command-name>` внутри args); census: все 43 однострочные | PASS (кроме нереального случая незакрытого тега, баг 1) |
| команда без ответа в хвосте получает «в работе…» как обычный промпт | probes (хвост, хвост + local stdout, tool_after reset); unit-тесты; filediff: +10 маркеров на реальных сессиях | PASS |
| `<local-command-stdout>` и прочие служебные префиксы скрыты в brief | differential по реальным данным: 0 изменений у записей без `<command-name>`, все сервисные категории скрыты; probes | PASS |
| обезличенная фикстура с реальной формой, без приватных данных | `slash_command.jsonl`: обе формы порядка тегов (name-first с `\n` + 12 пробелов, message-first с одиночным `\n`), cwd `C:\work\demo`, нулевые UUID, grep на пути/токены/длинные числа чистый; копия в scratch совпадает по hash | PASS |
| Existing tests pass | fmt, clippy -D warnings, test --workspace | PASS |

## 4. Bugs found

### 1. Low: незакрытый `<command-args>` теряет команду
`render.rs` `slash_command`: `rest.rsplit_once("</command-args>")?` возвращает `None`, если открывающий тег есть, а закрывающего нет. Запись уходит в service и скрыта в brief, хотя полностью отсутствующий `<command-args>` уже трактуется как пустые args.
- Repro: не-meta user запись `<command-name>/model</command-name><command-args>opus`.
- Ожидается: `> /model opus` (или хотя бы `> /model`). Фактически: brief пустой, full сырой.
- В 43 реальных записях такого нет, Claude Code всегда закрывает тег. Не блокирует.

### Замечание (не баг, по spec)
Локальная команда в хвосте (`/model`, `/login`, `/clear`) без ответа ассистента показывает «в работе…», хотя ничего не работает. На реальных данных это 10 сессий. Spec прямо требует такое поведение, так что это вопрос для follow-up, не для этого фикса.

## 5. Verdict

**SHIP.** Все критерии выполнены и проверены на всех 43 реальных командах обоих порядков тегов. Differential по 182 тыс. строк не показал ни одного изменения за пределами командных записей, служебные префиксы скрыты как раньше. Единственная находка касается формы записи, которой в реальных данных нет.

Контр-пример, который искал первым: реальная не-meta запись, которая начинается с `<command-name>`/`<command-message>`, но не является командой (обычный промпт стал бы `> /...`), либо реальная команда, которая остаётся скрытой. Не подтвердилось: 43/43 показаны, 0 изменений вне командных записей.

Cleanup: сервисы не поднимались. Удалены `scratch/qa/old/` (копия кода из main) и target-каталог харнесса в scratchpad.

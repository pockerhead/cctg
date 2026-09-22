# IMPL_REVIEW — TASK-006

Stage: code-reviewer (claude/opus, effort=medium). Проверен `git diff main -- Cargo.toml Cargo.lock crates/` на ветке `feature/transcript-renderers` (коммит aece25e).

## 1. Verdict

**PASS**: все критерии TASK_FINAL.md выполнены и проверены кодом, тестами и прогоном на 611 реальных транскриптах. Есть одна minor-проблема (compact summary в brief) и несколько nits, фикс желателен, но не блокирует.

## Disconfirmation

Контрпример, который я искал до оценки: вход, на котором `cut()` возвращает смещение, дающее кусок длиннее 4096 UTF-16 единиц (grapheme boundary правее `fit`), или нулевое смещение, из-за которого цикл `split_for_telegram` не завершается.

Результат: **не подтвердился**.
- `paragraph`/`line`/`space` записываются только для символов, которые уже влезли в лимит (`split.rs:62-75`), поэтому они ≤ `fit`.
- `last_grapheme_boundary(text, fit)` возвращает `fit` или `prev_boundary` от `fit`, то есть ≤ `fit`, и отфильтровывает 0 (`split.rs:97-106`).
- `fit` ≥ 4095 байт (символ занимает не больше 2 UTF-16 единиц), поэтому cut всегда > 0.
- Эмпирика: пробник `scratch/crev/probe` прогнал brief и full всех 611 jsonl из `~/.claude/projects` (1056 MB) через splitter. 0 кусков длиннее лимита, 0 пустых, 0 потерянного не-whitespace текста, каждый кусок это следующий слайс входа.

## 2. Confirmed correct

- **Идентичность плану.** SHA-256 всех 10 файлов совпадают с таблицей PLAN_FINAL / `scratch/reviewer2/proto_hashes.txt`. Корневой `Cargo.toml` +1 строка `unicode-segmentation = "1.13.3"`, `crates/transcript/Cargo.toml` +1 строка. Других файлов вне таблицы в diff нет.
- **Сборка и тесты** (CARGO_TARGET_DIR вне репо): `cargo fmt --check` чистый, `cargo clippy --workspace --all-targets -- -D warnings` чистый, `cargo test --workspace` даёт 54 passed (cctg 2, parse_fixtures 10, parse_tolerance 15, purity 3, render 11, split 13). Render suite 0.67 s в debug.
- **Зависимости:** transcript зависит только от `serde`, `serde_json`, `unicode-segmentation`. Это закреплено `purity.rs`. `unicode-segmentation` вне базового набора, но его явно приняли plan-reviewer-1/2 (log.jsonl), обоснование подтверждено пробниками.
- **`stop_reason` в парсере** (`lib.rs:55-57, 78, 162-166, 183`): `Value` плюс match на `Value::String`, так что неверный тип или null дают `None` и не роняют turn. Тест `parse_tolerance::stop_reason_is_tolerant`. Старые ожидания в `parse_fixtures.rs` не ослаблены, изменения только добавляют `stop_reason`.
- **Thinking не рендерится:** в модели `Block` нет варианта thinking, рендерер работает только с `Text/ToolUse/ToolResult` (`render.rs:61-118`). На реальных данных пробник нашёл 2 совпадения фрагментов thinking с выводом, оба ложные: пользователь вставил этот текст в свой промпт (строковый user record), и ассистент повторил мысль в обычном `text`-блоке.
- **Brief по одной строке на tool call** (`render.rs:89-99, 208-233`): `one_line` схлопывает переводы строк, входы и результаты только при `full`.
- **Brief скрывает промежуточный текст `tool_use`** (`render.rs:82-85, 151-157`): тест на реальной по форме фикстуре `final_answer_fixture_brief_and_full`. Fallback для null/другого stop_reason согласован в OPEN_DECISIONS (1).
- **Маркер «в работе…»** (`render.rs:121-123`): на реальных данных маркер стоит в 3 файлах, и ни в одном последняя assistant-запись не `end_turn` (0 ложных «в работе…» после законченного хода). Service-записи и скрытые meta состояние не меняют.
- **Нет квадратичности:** pre-pass'ы O(n) (`render.rs:161-206`), `cut()` сканирует не больше одного окна, grapheme-проверки идут только в точках кандидатов. Тест 5000/20000 turns с бюджетом 2 s и границей 8x. Самый большой реальный файл (94 MB) парсится и рендерится в обоих режимах за ~70 ms в release.
- **API для инкрементального пуша:** `render_brief(&[Turn])`/`render_full(&[Turn])` на произвольном слайсе, тест `slices_render_independently`.
- **Нет `unwrap`/`panic`/IO в `src/`**, закреплено `purity.rs`, включая проверку, что сканируются все файлы `src/`.

## 3. Issues

### Minor 1: compact summary рендерится в brief как промпт пользователя
- **Где:** `crates/transcript/src/render.rs:27-34` (`SERVICE_PREFIXES`), `render.rs:134-141`.
- **Что:** после `/compact` или автокомпакта Claude Code пишет не-meta `user` record со строковым content `This session is being continued from a previous conversation that ran out of context...` (~17k символов, флаги `isCompactSummary: true`, `isVisibleInTranscriptOnly: true`). `user_text` классифицирует его как `Prompt`, поэтому brief показывает его целиком как `> ...`. Кроме того, он сбрасывает `finished` и служит границей для `tool_after`. На этой машине так в 14 записях: 8 проверенных brief содержат summary, каждый даёт `prefer_file=true`, а одна такая запись в хвосте добавит в `/brief [n]` около 5 кусков текста, который пользователь не писал. Доменное правило «brief: user prompts» здесь нарушено по смыслу, хотя формальные AC проходят. Прямого теста или AC на это нет, поэтому это minor.
- **Как чинить:** проще всего добавить префикс `"This session is being continued from a previous conversation"` в `SERVICE_PREFIXES` (скрыт в brief, виден в full, состояние не меняет) и один кейс в `service_records_are_hidden_in_brief_and_keep_the_state`. Точнее было бы читать `isCompactSummary` в парсере, но это новое поле `Turn`, а тут хватит и префикса.

### Minor 2: отклонение от Step 0 плана по target dir (процесс, не код)
- **Где:** IMPL_SUMMARY.md §2: «Проверочные Cargo-команды использовали стандартный `repo/target`».
- **Что:** PLAN_FINAL Step 0 требует CARGO_TARGET_DIR вне репо. `target/` в корне репо существует, но он в `.gitignore`, поэтому в git ничего не попало. Отклонение подано как «нет отклонений», хотя это отклонение.
- **Как чинить:** ничего в коде; достаточно отметить в summary.

## 4. Missing coverage

- Compact summary record (см. Minor 1): brief не должен показывать его как промпт.
- Channel meta с `>` внутри значения атрибута (`<channel source="cctg" user="a>b">hi</channel>`): `split_once('>')` (`render.rs:145`) отрежет по первому `>`, и хвост атрибутов попадёт в тело промпта. Сейчас значения meta задаёт наш agent, риск низкий, но стоит зафиксировать тестом, когда TASK агента определит meta.
- Многострочный промпт: префикс `> ` ставится только на первую строку (`render.rs:71`). Теста, фиксирующего это поведение, нет.
- `\r\n` на границе куска: кандидат `space` после `\r` отбрасывается проверкой grapheme boundary (GB3), это правильно, но теста с CRLF-текстом около 4096 нет.

## 5. Nits

- `render.rs:111`: для `is_error` метка `error` заменяет имя инструмента (`← error: ...`). `← Bash (error): ...` было бы информативнее в full.
- `render.rs:71`: многострочный промпт выводится как `> line1\nline2`, визуально граница промпта теряется на второй строке.
- `render.rs:167` вызывает `user_text` второй раз на тех же записях, что и основной цикл. Это дёшево и O(n), просто дублирование.
- `scratch/crev/probe/` это мой пробник (только чтение `~/.claude/projects`, target в `%TEMP%`, реальных данных в репо нет).

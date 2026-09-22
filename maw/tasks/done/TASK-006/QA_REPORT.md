# QA_REPORT — TASK-006

Stage: qa (claude/opus, effort=medium). Ветка `feature/transcript-renderers`, HEAD `2c6faee` (код на `9c73880`). Код не менялся.

## Disconfirmation

Контрпример, который искал первым: вход, на котором splitter режет внутри extended grapheme cluster или теряет не-whitespace текст, когда кандидат мягкой границы (пробел/перевод строки) стоит прямо перед combining mark / ZWJ / вторым regional indicator, или когда граница 4096 падает в середину кластера после нечётного префикса флагов. Проверка шла через независимое API (`grapheme_indices` по всему исходному тексту, а не `GraphemeCursor` по остатку, как в коде).

Результат: **не подтвердился**. 3000 случайных входов (1-21 KB, 23 вида кусков: ZWJ-семьи, флаги, одиночный RI, keycap, skin tone, Devanagari, Hangul jamo, combining, CRLF, NBSP, `\t`), свип префиксов `a×4080..4099` + каждый кусок ×3 + 4 хвоста, в двух вариантах (с пробелом в первой половине и без), плюс `🇺` + 3000 флагов: 0 нарушений. Все разрезы стоят на границах графем исходного текста, кусок ≤ 4096 UTF-16, пустых/whitespace-only кусков нет, между кусками только whitespace, результат детерминирован.

## 1. Environment

Docker/Makefile/dev-сервера нет, библиотечный крейт без IO. Использован `cargo` напрямую, target вне репо.

```
export CARGO_TARGET_DIR="$TEMP/cctg-qa006-target"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript --edges normal --offline
```

Независимый пробник: `maw/tasks/in_progress/TASK-006/scratch/qa/probe/` (свой `[workspace]`, path-dep на `crates/transcript`, `unicode-segmentation = "=1.13.3"` для эталонной проверки графем).

```
export CARGO_TARGET_DIR="$TEMP/cctg-qa006-probe-target"
cd maw/tasks/in_progress/TASK-006/scratch/qa/probe && cargo build --release --offline
$CARGO_TARGET_DIR/release/qa_probe.exe fuzz|render|real
```

Режим `real` читает `~/.claude/projects/**/*.jsonl` только на чтение и печатает только счётчики. Реальный контент в репо не копировался. Сервисов не поднималось, чистить нечего (target-каталоги в `%TEMP%`).

## 2. Test results

**Существующий набор:** fmt exit 0, clippy `-D warnings` exit 0, `cargo test --workspace` 58 passed / 0 failed (cctg 1+1, parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14). Render suite 0.79 s в debug.

**Новые проверки (пробник QA):**

| Проверка | Результат |
|---|---|
| Fuzz splitter 3000 случаев + свип границы 4080..4099 × 23 куска × 4 хвоста × 2 | 0 нарушений |
| `🇺` + 3000 флагов (нечётный префикс RI) | 0 нарушений |
| Одна графема 9002 кодпоинта (`x` + 9000 combining) | 3 куска, каждый ≤ 4096, concat == вход |
| `""`, `" \n\t "` | 0 кусков, `prefer_file=false` |
| `max_chunks: 0` на "hi" | `prefer_file=true` (порог настраивается) |
| 52 KB: `a×4095` + 12000 🚀 | 7 кусков, первый 4095 units, второй начинается с 🚀, детерминирован, `prefer_file=true` |
| Thinking во всех формах: `thinking`, `redacted_thinking`, thinking+text в одном record, `text` с полем `thinking` и без `text`, `text: null`, `thinking` с полем `text`, thinking внутри `tool_result.content` | маркер не утёк ни в brief, ни в full |
| Brief vs full: `tool_use`-текст, input, result, `end_turn`-ответ | brief: только ответ и строка tool call; full: всё; маркера нет |
| Хвосты: `tool_use`-текст; только prompt; только thinking после prompt | `в работе…` в обоих режимах |
| Хвосты: `end_turn`; null без последующего tool call; `max_tokens` | без маркера (null/other это одобренный fallback) |
| `stop_reason` = null, 1, `{"a":1}`, `["end_turn"]`, `true` | turn сохраняется, `stop_reason = None` |
| Per-item tolerance: `[text:5, text:"ok", tool_use{id:null,name:null,input:null}]` | 1 turn, 2 блока (битый text отброшен поштучно) |
| `message: null`, `message: "x"` | 0 turns, без паники (поведение TASK-005, не менялось) |
| `render_brief(t[..2]) + "\n\n" + render_brief(t[2..]) == render_brief(t)` | true |
| **Реальные данные:** 613 jsonl, 1057 MB, brief+full+split | 0 нарушений split, 20293 куска, 754 рендера с `prefer_file`, 3 brief с маркером, 0 маркеров после финального `end_turn`, самый тяжёлый файл (96 MB) parse+brief+full за 73 ms release |
| Утечка thinking на реальных данных (80-символьные срезы thinking-блоков ≥ 200 символов) | 1 совпадение, ложное: тот же текст есть в обычном не-meta user prompt (пользователь вставил) |
| Compact summary: все 14 реальных `isCompactSummary` записей | все начинаются с нового service-префикса, значит фикс их покрывает |
| Timing test `five_thousand_turns_render_in_linear_time` | 10/10 последовательно, 48/48 при 16 параллельных ×3, 48/48 при 48 параллельных (3× переподписка на 16 ядрах). Флаки не увидел |
| Зависимость | `unicode-segmentation v1.13.3` без транзитивных зависимостей, в `Cargo.lock` +1 package с checksum `c6f5d3c3…a87a8`; `purity.rs` фиксирует ровно `serde, serde_json, unicode-segmentation`; в `src/` нет IO/unwrap/panic |
| Фикстуры `final_answer.jsonl`, `compact_summary.jsonl` | поиск путей пользователя, email, токенов бота: 0 совпадений |

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| Кусок ≤ 4096, не режет UTF-8 и суррогатные пары | fuzz + свип + реальные 613 файлов; длина в UTF-16 (консервативно ≥ кодпоинтов); все резы по `char`-границам и границам графем | PASS |
| Brief: одна строка на tool call без входов/результатов; full добавляет входы и усечённые результаты | пробник + тесты `brief_has_one_line_per_tool_call_and_no_io`, `full_truncates_long_results_safely`. Оговорка: имя инструмента или `agentId` с `\n` ломают "одну строку" (см. Bug 3) | PASS |
| Thinking не выдаётся ни в каком режиме | 7 синтетических форм + реальные данные; в модели `Block` нет thinking, `#[serde(other)] Ignored` | PASS |
| 50 KB блок и эмодзи на границе: детерминированные куски или "файлом" | тесты `fifty_kb_block_*`, `emoji_on_the_boundary_is_never_cut` + 52 KB emoji в пробнике | PASS |
| 5000 turns не квадратично, в бюджете | тест 2 s + 8× на 4× входе, прогнан 106 раз под нагрузкой без падений; реальный 96 MB за 73 ms | PASS |
| Публичный рендер для набора turns | `render_brief/render_full(&[Turn])`, конкатенация срезов совпадает | PASS |
| `Turn.stop_reason: Option<String>`, толерантность не ослаблена | 5 неверных типов → `None`, turn жив; per-item tolerance блоков цела; `string_or_default` и `RawBlock` в diff не менялись | PASS |
| Brief показывает только `end_turn`-текст, промежуточный `tool_use` скрыт, full показывает | пробник + `final_answer_fixture_brief_and_full`; null/other fallback одобрен (OPEN_DECISIONS 1) | PASS |
| Незавершённый хвост → `в работе…`, есть тест | пробник (3 вида хвоста) + `unfinished_tail_is_marked_in_progress`; на реальных данных 0 ложных маркеров | PASS |
| Existing tests pass | 58/58, fmt, clippy чистые | PASS |

## 4. Bugs found

Блокирующих нет.

**Bug 1 (minor): набранные пользователем slash-команды скрыты в brief.** `SERVICE_PREFIXES` содержит `<command-name>` и `<command-message>` с комментарием "written by Claude Code itself", но это запись о команде, которую пользователь сам набрал (`/maw-execute-task 6`, `/compact` и т.п.). На этой машине таких записей 43.
- Репро: `> q` / `done` (`end_turn`) / non-meta user `"<command-message>foo</command-message>\n<command-name>/foo</command-name>\n<command-args>6</command-args>"`.
- Ожидается: в brief видно `/foo 6` как prompt пользователя, а хвост без ответа помечен `в работе…`.
- Фактически: brief не показывает команду и не ставит маркер (`tail=(false,false)`, `brief_shows_slash=false`). Команда не сбрасывает `finished` и не служит границей prompt для null-fallback.
- Почему не блокер: список префиксов входит в одобренное решение оркестратора 2, AC это не покрывает. Стоит вынести пользователю: `<command-name>` скорее prompt, чем служебная запись. Лучше всего рендерить его как `> /foo 6`.

**Bug 2 (nit): пустой channel-prompt.** Meta `<channel source="cctg"></channel>` даёт строку `> ` и маркер `в работе…` после законченного обмена (`"> q\ndone\n\n> \nв работе…"`). Это `channel_body` → `Some("")`, пустое тело не отсекается. Реально возможно, только если agent прокинет пустое сообщение.

**Bug 3 (nit): "одна строка на tool call" держится только на честных данных.** `name` инструмента и `agentId` из `toolUseResult` в `tool_line` не проходят через `one_line`. Имя `"Na\nme"` или `agentId: "id\nwith\nnewline"` дают многострочный вывод. Claude Code таких значений не пишет, поле `description`/`subagent_type` схлопывается правильно (проверено, включая ` `, ` `, `\u0085`).

Проверено и не подтвердилось: квадратичность, флаки timing-теста, утечка thinking, разрез графем, потеря текста, ослабление tolerance после фикса, IO в новой зависимости. Фиксы из FIX_SUMMARY реальны: compact-префикс есть в `SERVICE_PREFIXES` (`render.rs:34`) и покрывает все 14 реальных записей, quote-aware `channel_body` правильно разбирает `user="a>b"` (тест `render.rs:249`).

## 5. Verdict

**SHIP.** Все 10 критериев TASK_FINAL проходят и по тестам автора, и по независимым проверкам (fuzz против эталонной сегментации, 1 GB реальных транскриптов, стресс timing-теста). Найденное это одно minor-расхождение в одобренном списке service-префиксов (slash-команды пользователя) и два nit на невалидных входах. Их можно взять отдельной мелкой задачей или вынести пользователю, мерж они не блокируют.

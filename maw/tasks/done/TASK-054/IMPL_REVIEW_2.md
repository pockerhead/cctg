# TASK-054 IMPL_REVIEW_2 (code-reviewer, round 2, fix 010b9c5 on top of 1ccecd6)

## 1. Verdict

**SHIP.** Все пять находок первого раунда закрыты, это подтверждено повтором repro и чтением кода. Регрессий уровня major/critical я не нашёл. Остались три minor-замечания: фоновые обновления статуса могут голодать при устойчивом foreground-спросе, промпт может ждать токен после сброса строк, окно ⏹ повторно взводится при повторном показе вопроса. Плюс пункт 6 первого раунда: нужен live soak, но это вне этой стадии.

## Disconfirmation

Проверял гипотезу: "ответ `Superseded` на 429 (новое в `dispatch`, `scheduler.rs:905-918`) может затереть правку с более новым намерением правкой с более старым. Например, решение по разрешению или вопрос ⏹ уступают обновлению статуса, посчитанному раньше. Либо `Shown.in_flight` не уменьшается, и статус слота замерзает навсегда."

**Не подтвердилась.**
- `enqueue` (`scheduler.rs:711-741`) держит в очереди не больше одной правки на сообщение. Поэтому любая правка того же сообщения, которая стоит в очереди, пока задача в полёте, была подана после того, как задачу взяли из очереди. Она новее по построению.
- `pump_status` (`slots.rs:6019-6022`) не подаёт обновление, пока правка слота в полёте (`in_flight == 1 && !urgent` → continue). Значит, обновление не может встать позади вопроса ⏹.
- Каждая поданная задача получает ровно один ответ: либо `Superseded` при коалесинге или 429, либо результат. `on_status_done` делает `saturating_sub(1)` до раннего return (`slots.rs:6121-6125`), так что `in_flight` возвращается в 0.
- Repro в %TEMP%: foreground-правка в полёте получает 429, за ней в очереди стоит более новый refresh. Первая получает `Superseded`, в Telegram уходит новый текст, и он остаётся foreground (`&=`). Лог: `scratch/repro_round2.rs.txt`.
- Остальные получатели правок (`permissions`, `questions`, `roster`, блоки субагентов) считают `Some(Ok(_))` применённым. Состояние они держат сами (`Edit::Due`/`retry`), а повтор строится из текущего состояния, не из старого `Op`. Устаревшего повтора поверх нового текста не бывает.

## 2. Первый раунд: закрыто ли

Repro первого раунда (`scratch/repro_edit_starvation.rs.txt`) адаптирован под новый API: статусы подаются как `refresh(..)`, то есть `background: true`, и добавлен callback. Прогон в %TEMP%-копии 010b9c5, копия удалена:

```
CREV2 n=2:  createTopic 10s, decision 10s, reaction 10s, callback 10s (submitted 10s); refresh max latency 8s
CREV2 n=3:  createTopic 10s, decision 12s, reaction 16s, callback 10s; refresh max latency 18s
CREV2 n=10: createTopic 12s, decision 16s, reaction 20s, callback 10s; refresh max latency 52s
```
Было в первом раунде: createTopic не ушёл за 170 с, decision шёл 42 с, reaction 46 с.

| # | Находка | Статус | Доказательство |
|---|---|---|---|
| 1 | Topic голодает | закрыто | repro выше; `next_edit` `scheduler.rs:786-800` чередует Topic и foreground, background идёт последним; тест `topic_calls_and_foreground_edits_never_wait_behind_status_refreshes` |
| 2 | Пользовательские правки за статусами, ⏹ не работает | закрыто | `Op::Edit::background`, true ставит только `pump_status` (`slots.rs:6073-6079`); вопрос ⏹ идёт foreground и встаёт на место ждущего refresh; `CONFIRM_FOR` отсчитывается заново с показа (`slots.rs:6185-6197`); тесты `a_stop_question_replaces_…`, `with_the_hubs_pacing_…` |
| 3 | Путь hub с `Limits::default()` не покрыт | закрыто | `slots::tests::with_the_hubs_pacing_topics_prompts_and_stop_stay_quick_under_status_churn` (11 слотов, `Limits::default()`, paused time) |
| 4 | Промпт обгоняет строки своей темы | закрыто | `next_permission` `scheduler.rs:644-682` отдаёт первую `merge`-строку темы промпта; `merge_lines` останавливается на промпте (`break` на не-Stream, `scheduler.rs:994-1003`) |
| 5 | `due()` ждёт строки за промптом | закрыто | `due` `scheduler.rs:844-858`: на любом не-joinable, включая промпт, сразу `return job.queued_at` |
| 6 | Live soak с `Limits::default()` | открыто (вне стадии) | Telegram и `.env` в этой стадии недоступны |

Прочее, что я проверил отдельно:
- `answerCallbackQuery` уходит сразу. `next_edit` сначала ищет `!edit_metered()`, bucket для этого не проверяется (`scheduler.rs:787-789`). В repro callback ушёл в 10 s при n=10. Ждёт только общую паузу после 429, так было и раньше.
- Коалесинг background/foreground: `*queued_background &= *background` (`scheduler.rs:726`), то есть foreground побеждает. Обратно, из foreground в background, правка переключиться не может. Refresh поверх ждущего вопроса ⏹ не встаёт, это блокирует `pump_status`.
- `roster.rs:167`: правка `/devices` по нажатию кнопки, `background: false`. Это ответ на действие пользователя, классифицировано верно. `roster.rs:410` это тест с `..`.
- Остальные `Op::Edit` в `slots.rs` (3542 Resume, 4573 expired, 4787 questions, 5011 decision, 6759 блоки) все `background: false`. Единственное место с `true` это `pump_status`.
- `urgent` не залипает: сбрасывается при `content == current` (`slots.rs:6058-6061`) или забирается `mem::take` при подаче правки.
- Сборка (`CARGO_TARGET_DIR` общий, `DEBUG=0`, `-j 1`, touch lib.rs/main.rs): `cargo fmt --check` ok, `cargo clippy --workspace --all-targets -D warnings` ok, `cargo test --workspace --no-fail-fast` EXIT=0, 946 passed, 0 failed (lib 746 + 1 ignored). Первый запуск упал на `failed to remove file target\debug\cctg.exe (os error 5)`: exe общего target держал параллельный прогон другого дерева. Повтор чистый, к диффу это не относится.
- Новых крейтов нет, stdout канала не трогается.

## 3. Issues

### 1. [minor] `scheduler.rs:793-799`: фоновые обновления статуса не ограничены по времени, если foreground-спрос не меньше бюджета

Background берёт токен только когда нет ни foreground-правки, ни Topic-вызова. Repro (`crev2_foreground_stream_starves_refresh`): bucket выбран, refresh подан на 0.1 s, реакции идут каждые 3 s (20/мин при 15/мин бюджета). Refresh ушёл только в **164 s**, после последней реакции. В жизни foreground это 👀/✍ на каждое входящее сообщение, правки решений и блоков субагентов, а каждый цикл разрешения даёт ещё 2 EditTopic + 2 Delete. Три-четыре промпта в минуту по разным сессиям уже приближаются к 15/мин. Статус при этом только замирает, данные не теряются. Утверждение в FIX_SUMMARY "every slot refreshed ≤ 90 s" проверено только при foreground 6/мин.
Как чинить: отдавать background каждый k-й токен (например, 1 из 4), если он ждёт дольше T. Либо повышать refresh, который ждёт больше ~60 s. Нужен тест с foreground ≥ 15/мин.

### 2. [minor] `scheduler.rs:699-702`: сброс строк перед промптом может стоить промпту токен (до 4 s)

Когда message-bucket на последнем токене, строку отдаёт `next_permission`, и она этот токен забирает. Промпт тогда ждёт refill. Repro: 5 сообщений в другую тему, строка на 4.0 s, промпт на 4.05 s. Промпт ушёл на **8 s**, а без строки ушёл бы на 5 s. По задаче это допустимо: промпт задерживает бюджет, а не дебаунс. Решение 4 оркестратора этот обмен и выбрало. Но стоит записать в doc модуля. Иначе можно отдавать строки первыми только когда в bucket ≥ 2 токенов.

### 3. [minor] `slots.rs:6185-6197`: окно ⏹ взводится заново при каждом повторном показе вопроса, не только при первом

Условие такое: "раньше показанный keyboard без вопроса, новый с вопросом, `confirm` есть". В нём нет проверки, что это первый показ после нажатия. Сценарий: ⏹ нажат, вопрос показан, окно до t+10. Пришёл промпт разрешения, статус без кнопок. Промпт решён до истечения окна, `status_view` снова рисует вопрос, и при его применении окно отсчитывается заново, ещё 10 s. С цепочкой промптов окно можно продлевать. Вопрос при этом виден пользователю, так что это не "подтверждение нажатия давнего прошлого вслепую". Но поведение шире, чем описано ("counted again from when Telegram shows the question" подразумевает первый показ). Как чинить: хранить в `confirm` флаг `shown_once` и перезапускать окно только один раз. Второй вариант: сбрасывать `confirm`, когда приходит промпт (`waiting`), так же как это делает нажатие во время ожидания.

### 4. [minor, open, перенос из раунда 1] Live soak с `Limits::default()` не сделан

Бюджет правок 20/мин по-прежнему догадка, см. FIX_SUMMARY §2.

## 4. Missing coverage

- Foreground-спрос ≥ 15/мин плюс N обновлений статуса: верхняя граница ожидания refresh (issue 1).
- Промпт при пустом message-bucket, когда у его темы есть ждущие строки: время промпта (issue 2).
- ⏹ → промпт разрешения → решение внутри окна → вопрос показан снова: сколько длится окно (issue 3).
- 429 на foreground-правке, пока за ней в очереди стоит background той же правки: результат `Superseded`, новый текст уходит foreground. Покрыт только обратный случай (`a_refresh_refused_with_429_gives_way_…`); мой repro показывает, что поведение правильное.

## 5. Nits

- `StatusJob::Create` при ответе ставит `next_at = now + every` (`slots.rs:6141`). Если ⏹ нажат, пока Create в полёте, вопрос ждёт до `status_every`, хотя `urgent` сохраняется. Так было и до фикса, и окно всё равно отсчитывается с показа.
- `with_the_hubs_pacing_…`: комментарий на строке 582 длиннее соседних (`… may take its turn first), inside its 10 s`), rustfmt его не переносит.

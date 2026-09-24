# Soak: несколько слотов на одном устройстве (TASK-018)

`crates/cctg/tests/soak.rs` прогоняет пять запусков на одном устройстве и проверяет, что темы, маршруты и реестр сходятся:

1. Две одновременные top-level сессии в папке A и одна в папке B: три темы `[host] A`, `[host] A #2`, `[host] B`.
2. Сообщения из каждой темы доходят только до агента своей сессии; `reply`, ответ хода (`Stop`) и строки транскрипта приходят в свою тему.
3. Вложенный `claude -p` внутри первой сессии A: темы нет, блок `⇣ nested` в теме родителя, в реестре `nested` с родителем. Вложенность находит сам `cctg hook` по дереву процессов.
4. Всплеск строк транскрипта в двух темах, два 429 от фейка, два permission-запроса во время очереди: запрос обгоняет строки своей темы, 429 останавливает всю очередь на `retry_after`, отказанный вызов повторяется один раз.
5. Первая сессия A завершается, hub останавливается, пятая сессия A стартует при выключенном hub (её `SessionStart` уходит в спул), hub возвращается: пятая занимает слот `[host] A`, один разделитель, новой темы нет.

В конце проверяются: ровно три `createForumTopic`, ни одного текста сессии в чужой теме, все `forum_topic_edited` удалены, `registry.json` поле за полем (три слота, текущие сессии, `pids`, вложенный запуск с родителем).

## Как устроено

Настоящие процессы `cctg hook` и `cctg agent`, настоящие `serve_hooks`, `serve_agents`, `Slots`, `Scheduler` и `updates::poll`. Claude Code заменён процессом-дублёром с именем `claude.exe` (копия тестового бинарника): он запускает хуки и агента своими детьми с тем окружением, что ставит Claude Code (`CLAUDE_PID`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_ENTRYPOINT`). Дублёр top-level сессии стартует через промежуточный процесс, который сразу выходит, поэтому цепочка процессов над ним обрывается, как у сессии из терминала, даже если тест запущен из Claude Code. Дублёр вложенного запуска это ребёнок дублёра A1, и хук находит родителя сам.

Home, `~/.cctg`, `CLAUDE_CONFIG_DIR` и состояние hub лежат во временной папке, настоящие `~/.cctg` и `~/.claude` не трогаются. Окон нет.

## Прогон с фейковым Telegram (по умолчанию)

```
cargo test -p cctg --test soak -- --ignored
```

Около 15-30 с. Без `--ignored` бинарник только печатает `soak: skipped`, поэтому обычный `cargo test --workspace` его не ждёт. Отчёт печатается в stdout; `CCTG_SOAK_REPORT=<абсолютный путь>` дополнительно пишет его в файл (относительный путь считается от `crates/cctg`, там cargo запускает тест).

## Живой прогон с настоящим ботом

Делает человек или оркестратор, не агент, который пишет код: тест сам читает `.env` (токен, чат, allowlist) и никуда его не выводит.

1. Остановить работающий `cctg hub`: два читателя `getUpdates` одного бота мешают друг другу (409), а тест подтверждает прочитанные апдейты.
2. Запустить из корня репозитория:

   ```
   CCTG_SOAK_LIVE=1 CCTG_SOAK_REPORT="$PWD/soak-live.md" cargo test -p cctg --test soak -- --ignored
   ```

   Другой файл настроек: `CCTG_SOAK_ENV=<путь>`. Прогон занимает несколько минут: бакет настоящий (20 сообщений в минуту), всплеск меньше, чем в фейке.
3. Проверить в группе: появились ровно три новые темы `[soakbox] A · a5a5a5a5`, `[soakbox] A #2 · a2a2a2a2`, `[soakbox] B · b1b1b1b1`; в них нет служебных сообщений "изменил название темы" или "изменил иконку" (кроме неудаляемого сообщения о создании темы); в теме A один разделитель `── session a5a5a5a5 · new ──`.
4. Удалить три темы `[soakbox]` вручную.

В живом режиме сообщения пользователя и нажатия кнопок тест подаёт hub напрямую (синтетические id), поэтому `setMessageReaction` и `answerCallbackQuery` на них отвечают ошибкой 400; в отчёте это "other errors". 429 в живом режиме не подстраиваются, только считаются, если случились.

## Шаблон отчёта

```
# TASK-018 soak report

mode: <fake Telegram | live (real bot)>; duration <s>; <N> Telegram calls; 2 hub runs

| operation | Bot API method | accepted |
|---|---|---|
| send | sendMessage (replies, answers, notices, separators, blocks) | |
| permission | sendMessage (permission prompts) | |
| stream | sendMessage (transcript stream) | |
| document | sendDocument | |
| edit | editMessageText | |
| react | setMessageReaction | |
| callback | answerCallbackQuery | |
| create_topic | createForumTopic | 3 |
| edit_topic | editForumTopic | |
| delete | deleteMessage (service messages) | |

- topics: 3 (A, A #2, B); separators: 1; service messages: <shown> shown, <deleted> deleted, 0 left
- 429: <n> (retry_after <s>), each followed by a pause of the whole queue and one retry; other errors: <n>
- permission latency (request written to the agent -> sendMessage): A <ms> (other topic), A #2 <ms> (own topic behind its stream); A #2 stream lines still queued behind its prompt: <n>
- stream: <lines> lines in <messages> messages; metered sends peak at <p>% of the bucket in any 1, 3 or 60 s window; smallest gap <ms>
- edits and topic calls are counted separately and are not compared with the 20 messages/min group limit: Telegram publishes no number for them
- registry.json: 3 slots (A: a5a5a5a5, A #2: a2a2a2a2, B: b1b1b1b1), nested ee0e0e0e -> parent a1a1a1a1
```

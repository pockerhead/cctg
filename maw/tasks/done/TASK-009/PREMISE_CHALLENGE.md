## Counter-example tested

Команды `/brief [n]` и `/full [n]` не содержат путь, а существующий hub не имеет иного доступного команде источника `transcript_path`; тогда заявленный вертикальный срез без реестра слотов не может выбрать локальный транскрипт, и критерии на фикстурах могут проверять лишь искусственно переданный путь, не реальное командное поведение.

## Primary-source investigation

- В `crates/cctg/src/hub/updates.rs:28-34` тип `Inbound`, передаваемый обработчику, содержит только `message_id`, `thread_id` и `text`; `classify` действительно конструирует ровно эти поля в `crates/cctg/src/hub/updates.rs:106-112`.
- В `crates/cctg/src/hub/config.rs:71-77` вся конфигурация hub состоит из `token`, `chat_id` и `allowlist`; в CLI у `hub` есть только `env_file` (`crates/cctg/src/main.rs:13-18`).
- Рабочий цикл пока только логирует входящее сообщение (`crates/cctg/src/hub/mod.rs:66-74`) и не содержит состояния сессии или пути. Команда `rg -n transcript_path crates\\cctg\\src || echo NO_MATCH` реально вывела `NO_MATCH`.
- Проверенный платформенный источник прямо устанавливает, что hook — единственный надёжный способ узнать session id и путь (`CLAUDE.md:47`), а предусмотренная привязка хранит `session_id -> (slot, transcript_path, ...)` (`CLAUDE.md:78`); чтение локального jsonl должно идти по полученному от hook `transcript_path` (`CLAUDE.md:92`).
- Запущена команда `cargo test -p cctg hub::updates::tests::allowlisted_text_is_input_and_strangers_are_dropped -- --exact`; реальный результат: `1 passed; 0 failed`, то есть существующий тест подтверждает именно контракт `Inbound { message_id, thread_id, text }`, показанный в `crates/cctg/src/hub/updates.rs:244-256`.

## Did it hold

Да. Для реального апдейта `/brief [n]` или `/full [n]` hub получает тему и текст команды, но в доступных команде входах нет ни `transcript_path`, ни связи темы с сессией. При этом первичный платформенный источник исключает надёжное восстановление пути из самого Channel/Telegram-ввода. Поэтому заявленные тесты на фикстурах способны пройти с внедрённым тестовым путём, хотя пользовательская команда в работающем hub всё ещё не сможет определить, какой локальный транскрипт читать.

## Verdict

PREMISE SUSPECT — `Inbound` не несёт пути (`crates/cctg/src/hub/updates.rs:28-34`), иных источников пути в hub нет (`rg -n transcript_path crates\\cctg\\src || echo NO_MATCH` → `NO_MATCH`), а проверенный источник требует получать его из hook и связывать с сессией (`CLAUDE.md:47,78,92`) ; smallest implied reframing: критерий успеха должен проверять получение `transcript_path` для реального апдейта `/brief [n]`/`/full [n]`, а не только рендеринг по внедрённому пути фикстуры.

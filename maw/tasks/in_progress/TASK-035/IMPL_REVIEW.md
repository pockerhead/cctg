# TASK-035 IMPL_REVIEW (code-reviewer)

Ревью кода `git diff 50a9408 c3128ab -- . ':!maw'` (44 файла, +3948 -238) на `feature/remote-hub`. HEAD `018f649` отличается от `c3128ab` только файлами `maw/`.

## 1. Verdict

**SHIP.** Критических и major-дефектов в коде не нашёл. Проверил: TLS и pin, запрет открытого TCP на удалённый адрес, ограничения до проверки секрета, flush в хуке, сборку = коммит, прокси, Dockerfile и workflows. Замечания ниже minor, их можно закрыть fixer-ом. Одно условие остаётся, и его здесь снять нельзя: Linux, macOS и образ впервые проверит первый прогон `ci` (Docker и WSL выключены, Mac нет).

## Disconfirmation (сделано до оценки)

Самый конкретный контрпример: **какой-то путь устройства отправляет секрет открытым TCP на адрес не на этой машине.** Например, есть обход мимо `DeviceConfig::hub`, pin сломан и срабатывает откат, или проверка loopback принимает имя вроде `127.0.0.1.nip.io`. Второй контрпример: **pin принимает цепочку, где с pin совпадает не leaf, или сертификат с совпавшим pin без его ключа.**

Что проверил:
- Grep по `TcpStream::connect`, `hook_addr`, `agent_addr`, `hook::post`, `hook::ask`, `spool::replay` в `crates/cctg/src`. Все пути, где идёт секрет, строят адрес через `DeviceConfig::hub`: `hook.rs:124,174`, `agent.rs:574-575`, `statusline.rs:75`. Вне тестов raw `TcpStream::connect` остался только в `tls.rs:256` (внутри `HubAddr::connect`) и в `hub/mod.rs:172` (health, ни одного байта).
- `device.rs:136-143`: сломанный pin возвращает `Err(BadPin)`, открытый TCP только при `is_loopback_addr`. `tls.rs:190-196` принимает `localhost` или IP-литерал с `is_loopback()`. `127.0.0.1.nip.io` не парсится как IP, значит это remote. `[::ffff:127.0.0.1]` тоже remote (`Ipv6Addr::is_loopback` = false), это консервативно.
- `tls.rs:116-131`: `verify_server_cert` сверяет только `end_entity`. Подпись рукопожатия проверяется через `rustls::crypto::verify_tls13_signature` с алгоритмами провайдера. TLS 1.3 only с обеих сторон (`tls.rs:202`, `tls.rs:378`). Тест `the_pinned_certificate_without_its_key_does_not_pass` (поддельный сервер с чужим ключом) проходит.

Итог: оба контрпримера не подтвердились. Остаток по имени `localhost` описан в п. 6.

## 2. Confirmed correct

Прогоны на Windows (общий target, `-j 1`):
- `cargo fmt --all -- --check`: чисто. `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: чисто.
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/code-reviewer/workspace_test.txt`): 755 passed, 4 failed, 3 ignored. Все 4 падения из-за чужой пересборки общего `target/debug/cctg.exe`. `update_e2e` показал `left: "15908a30…-dirty.3a9c…"` против нашего `018f649…`. Три тайминговых теста `hook_cli` (1.3-2.4 с) упали на том же только что подменённом exe. После `touch main.rs` повтор зелёный: `hook_cli` 8/8, `update_e2e`, `tls_e2e`, `proxy_e2e` (`scratch/code-reviewer/rerun.txt`). Этот повтор шёл на чистом коммите `018f649`, то есть это как раз случай CI, когда `SOURCE` равен коммиту. `update_e2e` проходит и в нём.

По коду:
- **TLS/pin**: `tls.rs`. Только leaf, подпись проверяется, TLS 1.3 only, `CertPin::parse` не пропускает `+f` (`tls.rs:65-69`). Ошибки PEM с фиксированным текстом (`tls.rs:339-348`). `HubAddr` в Debug показывает только адрес и флаг TLS.
- **Без открытого отката**: `device.rs:136-143`, `agent.rs:566-582` (удалённый адрес без pin даёт `NoHub::NoConfig`, тест `link_plan_rules`). `hook.rs:124-130`: ошибка конфигурации даёт одну строку stderr, событие не спулится.
- **Ограничения до секрета** (`hub/ingress.rs`):
  - дедлайн считается от accept и включает TLS: `ingress.rs:198,591`, `open` использует `timeout_at` (`ingress.rs:124`);
  - `MAX_PENDING_AGENTS` через `try_acquire_owned` до спавна (`ingress.rs:187`), разрешение отдаётся после аутентификации (`ingress.rs:314`);
  - hello до 4 КиБ (`ingress.rs:266`, `wire.rs:727-740`), заголовки хука до 8 КиБ, тело только после секрета (`ingress.rs:893-909`);
  - пауза 250 мс на обоих слушателях (`ingress.rs:301-304,639-642`), сравнение в постоянное время (`wire::Secret::matches`);
  - пустые соединения и сбои TLS пишутся debug (`ingress.rs:126-133,296,621`);
  - до аутентификации нет `unwrap`/`expect`, fuzz-тест на 2000 входов.
- **Flush в хуке по TLS**: `hook.rs` в `post` и `ask`, есть регрессионный тест на 128 КиБ.
- **Бюджеты хуков**: plain 500/300/500 не изменились, TLS 900/600/1000, `STDIN + TLS_POST ≤ 1200 мс` покрыто тестом.
- **Сборка = коммит**: `build.rs` (валидация `CCTG_BUILD_ID`, `rerun-if-*` только на существующие пути), `client::identity` (чистый коммит без хеша, `-dirty.<hash>`, без git хеш). Кейс PREMISE покрыт `slots::tests::one_commit_on_two_systems_is_current_and_another_commit_is_warned_once` и `client::tests`. CI передаёт `CCTG_BUILD_ID=${{ github.sha }}` образу, клиенту smoke и релизным клиентам. Локальная сборка из чистого checkout даёт `git rev-parse HEAD`, то есть тот же sha. `deploy.rs:228` проверяет только префикс `cctg `, новый формат `--version` его не ломает.
- **Прокси**: reqwest не отключает системный прокси (`api.rs:259`, нет `.no_proxy()`). Одна info-строка без значения (`hub/mod.rs:249-252`). `proxy_e2e` зелёный (CONNECT с учёткой, абсолютная форма, учётка не попадает в вывод). Агенты и хуки ходят raw TCP/TLS, прокси не используют.
- **Dockerfile**: `musl-dev gcc linux-headers`. В `aws-lc-sys-0.45.0/builder/main.rs:916-923` у `x86_64-unknown-linux-musl` есть pregenerated bindings, `cc_builder/linux_x86_64.rs` существует, `ring` в нормальном дереве отсутствует (`cargo tree -i ring -e normal`: пусто). uid 10001, `/data` принадлежит cctg, `HEALTHCHECK cctg health`. Runtime `alpine:3.21` включает `ca-certificates-bundle`, а он нужен `rustls-platform-verifier` → `rustls-native-certs`. Теги `rust:1.95-alpine`, `alpine:3.21`, `nickfedor/watchtower:latest`, `python:3.12-alpine` есть на Docker Hub (HTTP 200).
- **`.dockerignore`**: allowlist `*` / `!Cargo.toml` / `!Cargo.lock` / `!crates/`. `.env`, `.git`, `target`, `maw`, `.github` в контекст не попадают.
- **Workflows**: `permissions: contents: read` на уровне workflow, `packages: write` только у `image`, `contents: write` только у `release`, `persist-credentials: false`. Кроме `GITHUB_TOKEN` секретов нет. Все 8 SHA сверил через `gh api repos/<action>/commits/<tag>`, все совпали. Бинарники собираются только на `v*`. macOS arm64 есть в CI (`macos-latest`) и в релизе (`macos-14`).
- **Локальная Windows-схема**: адреса по умолчанию `127.0.0.1`, открытый TCP, прежние бюджеты. Все прежние e2e идут через `HubAddr::plain`.

## 3. Issues

Critical: нет. Major: нет.

1. **minor, `hub/ingress.rs:41,187-190`: глобальный лимит в 16 неаутентифицированных агентов, дешёвое выбивание.**
   Сценарий: хост из интернета держит 16 TCP-соединений без байта и обновляет их каждые 5 с (примерно 3 connect/s). Каждое новое соединение настоящего агента закрывается сразу, агенты уходят в backoff до 30 с, Telegram-темы перестают получать ответы. До задачи порог был 256.
   Доказательство: тест `unauthenticated_agents_beyond_the_cap_are_closed_at_once` сам показывает, что 17-е соединение закрывается сразу, кто бы его ни открыл. В `docs/remote-hub.md:138` это записано как остаточный риск, и решение plan-reviewer-2 отвергло лимит по IP.
   Fix (по желанию): учёт `HashMap<IpAddr, u8>` в `serve_agents` (например, не больше 4 ожидающих с одного IP), около 15 строк. Или оставить как есть и явно сказать в доке, что порог 16.

2. **minor, `hub/ingress.rs:300,310,581,635,644` (и `192`): WARN на каждое неаутентифицированное соединение с мусором.**
   Сценарий: сканеры, которые проходят TLS (Censys, Shodan после рукопожатия шлют `GET /`), дают на хук-порту `warn "hook request rejected" status=405`, на агент-порту `warn "agent rejected" reason=Protocol`. Медленные дают `warn "... timed out"`. На plain-слушателе (hub на своей машине с не-loopback адресом) WARN даёт любой HTTP-сканер. Частота ничем не ограничена, и настоящие предупреждения тонут в шуме. Ротация 10m×3 в compose ограничивает только диск.
   Доказательство: код по строкам выше. В debug уходят только пустые соединения и сбои TLS (`ingress.rs:126-133,296,621`), и `docs/remote-hub.md:136` честно говорит только о них.
   Fix: до аутентификации писать `Protocol`/`NotFound`/`MethodNotAllowed`/таймауты на debug, на warn оставить только `Auth`. Или ограничить warn одной строкой в минуту.

3. **minor, `.github/smoke/smoke.py:192-197`: smoke может зависнуть, а не упасть.**
   Сценарий: сообщение не дошло до агента. `agent.stdout.readline()` блокируется без таймаута, проверка `deadline` стоит только между строками, агент больше ничего не печатает. Задача `image` висит до `timeout-minutes: 60` вместо понятного `FAIL`.
   Fix: читать stdout в отдельном потоке в очередь с `queue.get(timeout=...)`, либо `select`/`os.set_blocking(False)` с опросом до дедлайна.

4. **minor (док), `deploy/compose.yml:77` + `docs/remote-hub.md:18-20,49-55` + `release.yml:48`: при первом развёртывании `:latest` ещё не существует.**
   Сценарий: пользователь делает ровно то, что в доке. Первый пуш в main, пакет стал публичным, затем «Один раз на сервере» и `docker compose up -d`. Тег `latest` публикует только `v*` (`release.yml:48`), после пуша в main есть лишь `:main` и `:sha-…`, и compose падает с `manifest unknown`.
   Fix: в «Один раз в GitHub» добавить шаг «поставить первый тег `v0.1.0` на зелёный коммит» или сказать, что до первого релиза в `compose.yml` нужно указать `:main`.

5. **minor (док), `docs/remote-hub.md:94,122`: Linux-клиенту из Releases нужен `chmod +x`, а док говорит его только для macOS.**
   Сценарий: `curl -LO .../cctg-v1-x86_64-unknown-linux-musl` скачивает файл без бита исполнения, и `mv` поверх старого бинарника даёт `Permission denied` при следующем запуске claude (агент и хуки не стартуют). Это то же, что `tests/common::write_program` чинит для тестов (PCTX-предложение 6).
   Fix: в разделах «Устройство» и «Версии клиента и hub» дописать `chmod +x` для Linux.

6. **minor (нит по безопасности), `tls.rs:190-196`, `device.rs:140`: `localhost` без pin считается loopback по имени.**
   Сценарий: адрес `localhost:47292` разрешает резолвер ОС. Если в `/etc/hosts` или у резолвера `localhost` указывает на другой адрес (намеренно или по ошибке), секрет уходит открытым текстом за пределы машины. Риск низкий: glibc и Windows резолвят `localhost` локально, RFC 6761.
   Fix: в `HubAddr::connect` для plain проверять, что `peer_addr()` у TCP действительно loopback. Или принимать для plain только IP-литералы.

7. **minor, `client.rs:77-85` + `hub/slots.rs:4029-4035`: две разные грязные сборки одного коммита в предупреждении выглядят одинаково.**
   Сценарий: у hub `X-dirty.H1`, у агента `X-dirty.H2`, это разные сборки, предупреждение правильное. Но `short()` у обоих даёт `XXXXXXXX-dirty`, и в теме написано «клиент 01234567-dirty, hub 01234567-dirty». Пользователь не видит, в чём разница.
   Fix: для `-dirty` добавлять 4-6 символов хеша, например `01234567-dirty.3a9c`.

8. **minor (тесты), `tests/update_e2e.rs:106-110,176,246`: при чистом коммите тест больше не доказывает, что новый worker сообщает новую сборку.**
   В CI `SOURCE` равен коммиту, `build_id_of` старого и нового файла одинаковый, и оба `assert_eq` пройдут, даже если hub-у представится старый worker. Что файл действительно сменился, доказывает только `UpdateOutcome::Reloading`.
   Fix: дополнительно сравнить `update::Worker`/файловый хеш нового worker-а, или проверить `second != first` плюс то, что новый процесс запущен из нового файла (например, по `cctg --version` копии).

## 4. Missing coverage

- Нет теста, что plain-путь не уходит на не-loopback адрес при `localhost` с не-loopback разрешением (п. 6). Тест с подменой резолвера сложен, но проверку `peer_addr` после connect легко покрыть unit-тестом.
- Нет теста, что pre-auth отказы (`Protocol`, 405/404, таймаут) не пишут WARN, если п. 2 будет исправлен. `ingress_logs.rs` проверяет только пустые соединения.
- `release.yml` ни разу не прогонялся (нужен тег). Пути `mv dist/cctg …`, `merge-multiple` и `gh release create` проверяются только чтением. Можно один раз прогнать на тестовом теге `v0.0.0-rc1` в форке.
- Linux: причина прежних падений `files_e2e`/`slots_logs` не подтверждена (решение 12 PLAN_FINAL). Правка только тестовая: синхронизация по строке лога. Если CI снова покраснеет, это указание на продуктовую гонку `Registered` → bind против входящего из темы. Разбирать по правилам PLAN_FINAL §3, не ослабляя ожидания.
- macOS: clippy `-D warnings` на `macos-latest` не прогонялся. `proctree` проверил по cfg (`claude_pids` используется через `live_claude_pids`, `parse_stat` под `allow(dead_code)`), но полную уверенность даст только CI.

## 5. Nits

- `release.yml:115-117`: `release` не зависит от зелёного `ci` этого коммита, это держится только на тексте дока («Tag only a commit whose ci is green»). Можно добавить job `test` в `needs` или проверку статуса через `gh api .../check-runs`.
- `build.rs:84`: `git rev-parse --path-format=absolute` появился в git 2.31. На старом git список наблюдения пуст, и коммит без изменения исходников (Y поверх X-dirty) оставит в бинарнике `X-dirty`, пока не изменится исходник. Для dev-машины это неважно.
- `rustls`/`tokio-rustls` новые прямые зависимости вне «agreed set» из tooling note. Задача прямо просит TLS, оба крейта уже были транзитивно через reqwest, новых крипто-стеков нет. Принято.
- `hub/mod.rs:231-248`: слушатели привязаны до `getMe`, поэтому healthcheck зелёный и тогда, когда Telegram недоступен. Док это честно говорит (`remote-hub.md:59`), просто отмечаю поведение.

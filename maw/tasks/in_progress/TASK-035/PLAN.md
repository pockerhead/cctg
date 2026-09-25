# TASK-035 PLAN: hub на удалённом сервере (TLS, Docker, CI, сборка = коммит, прокси Bot API)

Эталон: `maw/tasks/in_progress/TASK-035/scratch/planner/` (дальше `P/`). Весь дизайн собран и прогнан в копии workspace вне репозитория (`%TEMP%/cctg-035-ref`: `git archive` HEAD `23e06e0` + `CLAUDE.md` + правки; сборки в общем `target` репозитория, `-j 1`; копия удалена после прогона). Код в `crates/` между `23e06e0` и текущим HEAD `8a27905` не менялся (тот коммит трогает только `OPEN_DECISIONS.md`), patch применим к HEAD. Implementer применяет patch, сверяет hashes и прогоняет проверки; шаги ниже объясняют каждое изменение для ревью.

- `P/task035.patch`: diff от HEAD (LF). Применять `git apply --ignore-whitespace maw/tasks/in_progress/TASK-035/scratch/planner/task035.patch` из корня (checkout с `core.autocrlf=true`). `P/hashes.txt` + `P/verify_hashes.sh`: sha256 файлов после патча (CR вырезается). `P/build_patch.py <ws>` пересобирает patch и hashes из workspace (сам эталонный workspace удалён; patch, применённый к свежему `git archive HEAD`, дал ровно его содержимое и все hashes OK).
- Прогоны эталона (Windows): `P/workspace_test.txt` (`cargo test --workspace --locked --no-fail-fast`), `P/clippy.out.txt` (`clippy --workspace --all-targets --locked -D warnings`), `P/fmt.out.txt`, `P/mutations/mutations.log` (12 мутаций, все убиты; скрипт `P/mutations/run_mutations.py <ws> [имя...]`).
- HEAD без правок на Windows: `P/baseline-clippy-windows.log` (clippy чисто), fmt чисто.
- Linux и Docker: `P/linux-baseline.log` (HEAD до правок, clippy + все тесты в `rust:1.95-bookworm`). Контейнерная проверка образа подготовлена, но не прогнана: `P/docker/` (`run_docker_e2e.py`, `compose.test.yml`, `fake_bot.py`). Что прогнано, а что осталось CI, в разделе 0.
- Решения с отвергнутыми вариантами: `OPEN_DECISIONS.md` пункты 2-12 (planner). Предложения в project context: `PCTX_PROPOSALS.md`.

## 0. Что проверено и чем

Windows (эталон, финальный прогон): fmt чисто, clippy `-D warnings` чисто, `cargo test --workspace` зелёный (40 тестовых бинарников, lib `cctg`: 579 passed, 1 ignored; новые `tls_e2e`, `proxy_e2e` проходят). Мутации (`P/mutations/mutations.log`): pin принимает любой сертификат, TLS 1.3 подпись не проверяется, plain на любой адрес, сломанный pin откатывается в plain, соединение игнорирует TLS, чистый коммит хеширует файл, грязная сборка без хеша, `ReadTask` не прерывает чтение, одна из двух TLS-переменных молча игнорируется, пустое соединение пишет warning, Bot API без прокси, URL прокси в логе: все 12 убиты.

Linux, прогнано в Docker до того, как пользователь остановил Docker Desktop и WSL (сообщение координатора: не запускать снова):
1. `P/linux-baseline.log`: HEAD без правок в `rust:1.95-bookworm`, clippy и `cargo test --workspace --no-fail-fast`. Clippy падает (`hub/testdir.rs:79` unused import, `proctree.rs:40-72` dead code). Тесты: 5 падений: `statusline::tests::git_bash_is_found_like_claude_code_finds_it` (`JoinPathsError`: `C:`-пути не склеиваются в Unix PATH), `supervise_e2e` и `update_e2e` (`PermissionDenied`: скопированный бинарник без execute bit), `slots_logs` (заголовок не дошёл до темы), `files_e2e` (строка hub не пришла за 30 с, `files_e2e.rs:328`).
2. Clippy эталона на Linux (частично): после первых правок нашёл ещё unused `HashMap` в `proctree.rs:14`; исправлено, повторно на Linux не прогнано.
3. `actionlint` (образ `rhysd/actionlint`) по обоим workflow: 0 замечаний. `hadolint`: только DL3018 (версии apk), подавлено с комментарием.

Не прогнано из-за остановки Docker, остаётся CI (`.github/workflows/ci.yml`) или `P/docker/run_docker_e2e.py`, если пользователь снова включит Docker:
- тесты эталона на Linux (включая `tls_e2e`, `proxy_e2e` и две гипотезы про `slots_logs`/`files_e2e`, шаг 3.14);
- `docker build` образа (список пакетов сборщика `musl-dev gcc` взят из документации aws-lc-rs, на деле не собирался), compose со здоровым healthcheck, агент и хук с Windows по TLS в контейнер, отсутствие секретов в образе, одинаковая сборка Linux-hub и Windows-клиента одного `CCTG_BUILD_ID`.

Хвосты Docker на этой машине (удалить, когда Docker снова работает): контейнер `cctg035-dev`, тома `cctg-035-cargo-registry`, `cctg-035-linux-target`, образы `rust:1.95-bookworm`, `rust:1.95-alpine` (скачан не до конца), `rhysd/actionlint`, `hadolint/hadolint`.

## 1. Understanding (что есть сейчас)

- Адреса прослушивания уже настраиваются (TASK-010): `crates/cctg/src/hub/config.rs:20-27` `CCTG_AGENT_LISTEN`/`CCTG_HOOK_LISTEN` (по умолчанию `127.0.0.1:47291/47292`), `:178-184` разбор `ip:port`. `hub/ingress.rs:49-57` `bind` предупреждает о не-loopback «plain TCP».
- Hub: `hub/mod.rs:163-285` `run`: config, state, `ingress::bind` двух слушателей (`:175-180`), `getMe` с тремя повторами (`:191-203`), `own_build()` (`:216-224`), `Slots`, `serve_agents` и `serve_hooks_and_permissions` (`:253-265`), poll до остановки. TLS нет.
- `hub/ingress.rs`: `serve_agents` `:83-113` принимает `TcpListener`, на соединение `agent_session(TcpStream)` `:145-264` (`into_split`, handshake 5 с, reader-задача `read_agent_frames` `:266-284` через `tokio::spawn`); hooks: `serve` `:431-467`, `hook_request(TcpStream)` `:469-502`, `respond` `:505-520`, `permission_request` `:526-576`, `gone` `:580-588`, строгий `read_request` `:665-765` (пустое соединение даёт 400 и warning).
- Устройство: `device.rs:59-111` `DeviceConfig { secret, hook_addr, agent_addr, host, state_dir }` из env и `~/.cctg/device.env` без `set_var`. Клиенты: `hook.rs:632-670` `post(addr: &str)` и `:253-289` `ask(addr: &str)` открывают `TcpStream::connect(addr)`; бюджеты `:52-87` (500/300/500 мс); `spool.rs:242-265` `replay(addr: &str)`; `statusline.rs:32,61-75` (80 мс); `agent.rs:117-135` `LinkConfig { addr: String }`, `Replay { hook_addr: String }`, `:255-281` `connect` (`TcpStream` + `into_split`), `:286-369` `serve`/`read_hub_frames`, `:545-562` `link_plan`.
- Идентичность сборки (TASK-040): `client.rs:1-51` сборка = sha256 своего exe; `wire.rs:169-182` `Client { version, build, self_update }`; `hub/slots.rs:999-1010` лог «agent runs another cctg build», `:3991-4001` `outdated` = `client.build != hub`, `:4005-4056` `warn_outdated` (один громкий ⬆️ на сборку hub), `:4258` `NO_NEW_BUILD_NOTICE`; `hub/status.rs:62-63,105-112` тексты. `update.rs:131-151` `Worker::plan` сравнивает файл на диске с хешем своего файла (остаётся). Linux-hub и Windows-клиент одного коммита сегодня всегда «устаревшие» (PREMISE_CHALLENGE).
- `main.rs:7` `#[command(version)]` печатает только `0.1.0`; подкоманды `:13-71`, `health` нет.
- Bot API клиент: `hub/api.rs:259-263` `reqwest::Client::builder()` без `no_proxy()`: reqwest 0.13 сам читает `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY`/`NO_PROXY` (hyper-util `Matcher::from_system`, localhost не обходится). `ApiError::Http` снимает URL запроса (`api.rs:39-42`), Display reqwest-ошибки источник не печатает.
- Сборки: один бинарник `cctg` (hub, agent, hook, supervise, deploy, run). Docker-файлов и `.github/` нет. Windows-тесты на Linux ни разу не запускались.

## 2. Approach

1. **TLS на тех же двух слушателях.** Новый модуль `crates/cctg/src/tls.rs`: `Stream` (enum Plain/TLS поверх `TcpStream`, `AsyncRead`/`AsyncWrite`), клиентский `HubAddr` (plain или TLS 1.3 с pin), серверный `Acceptor` из PEM (`CCTG_TLS_CERT`, `CCTG_TLS_KEY`), `Incoming` для слушателя, `ReadTask`. Протоколы не меняются: newline-JSON и рукописный HTTP/1.1 идут поверх `Stream`. rustls 0.23 и tokio-rustls 0.26 уже в lock через reqwest, провайдер aws-lc-rs тот же. WebSocket отвергнут (решение 2).
2. **Проверка сертификата на устройстве = pin.** `CCTG_HUB_CERT_SHA256` (sha256 DER-сертификата hub). Свой `ServerCertVerifier` принимает только эти байты, подпись рукопожатия проверяет стандартно (`rustls::crypto::verify_tls13_signature` с алгоритмами провайдера), так что сертификат без ключа не проходит (мутация `tls13-signature-not-checked` убита тестом с поддельным сервером). Так устроен pinning у Syncthing; rustls требует для этого `dangerous()` API, но проверка не отключается. Системные корни отвергнуты (решение 3).
3. **Когда TLS:** pin задан = TLS на обоих каналах; без pin открытый TCP только на loopback, иначе `ConfigProblem::PlainRemote` и ни байта секрета в сеть (решение 4). Сломанный pin не откатывается в plain.
4. **Сборка = коммит.** `crates/cctg/build.rs` кладёт в `CCTG_SOURCE`: `CCTG_BUILD_ID` (CI, Docker) или `git rev-parse HEAD` (+`-dirty`, если `crates/`, `Cargo.toml`, `Cargo.lock` отличаются). `client::identity`: чистый коммит как есть; `-dirty` плюс sha256 exe; без git sha256 exe (как раньше). `Client.build` остаётся тем же полем wire (строка непрозрачна, VERSION не растёт). Локальная проверка «новый файл на диске» остаётся на хеше файла (решение 6).
5. **Один бинарник** (решение 5). Образ: статический musl на `rust:1.95-alpine`, runtime `alpine:3.21`, uid 10001, том `/data`, `HEALTHCHECK cctg health`; тот же stage отдаёт Linux-бинарник для Releases (решение 8). `cctg health` = TCP-connect к обоим слушателям на loopback, без байта; hub пишет такие соединения и неудачные TLS-рукопожатия только в debug (решение 7).
6. **CI и выкатка:** `ci.yml` (fmt, clippy, test на ubuntu и windows; сборка образа без push), `release.yml` (образ в GHCR на push в main и на тег, бинарники Windows и Linux в Releases на тег, только `GITHUB_TOKEN`). На сервере `deploy/compose.yml` (+ профиль `autoupdate` с `nickfedor/watchtower`, форк заархивированного `containrrr/watchtower`) или `docker compose pull && up -d` (решение 9).
7. **Прокси Bot API** (решение 2 orchestrator, решение 11 planner): оставить стандартное поведение reqwest, одна строка в логе без значения, `extra_hosts: host.docker.internal:host-gateway` в compose и `deploy/compose.host.yml` (сеть хоста для прокси на `127.0.0.1`). Тест со заглушкой прокси.
8. **Linux-тесты:** точечные `cfg`, execute bit копий, точки синхронизации в двух тестах с гонками (решение 12).

Исследование (источники):
- rustls `ServerCertVerifier` (docs.rs rustls 0.23): четыре обязательных метода; `verify_tls12/13_signature` проверяют подпись рукопожатия, это и держит pin честным.
- rustls-pki-types `PemObject` (docs.rs 1.15.1): `CertificateDer::pem_file_iter`, `PrivateKeyDer::from_pem_file`; ошибка `pem::Error::IllegalSectionStart { line }` несёт строку файла, поэтому ошибки PEM заменяются фиксированным текстом (`TlsFileError`).
- aws-lc-rs, требования Linux (https://aws.github.io/aws-lc-rs/requirements/linux.html): вне FIPS нужен только C/C++ компилятор, CMake не нужен.
- Cargo build scripts (https://doc.rust-lang.org/cargo/reference/build-scripts.html): `rerun-if-changed` на каталог сканирует его целиком; без директив скрипт перезапускается от любого файла пакета; `rustc-env` читается `env!`.
- GitHub Docs «Publishing Docker images» (https://docs.github.com/en/actions/tutorials/publish-packages/publish-docker-images): `permissions: packages: write`, `docker/login-action` с `GITHUB_TOKEN`, `metadata-action`, `build-push-action`. Версии actions сверены по GitHub API 2026-09-25: checkout v7, upload-artifact v7, download-artifact v8, build-push v7, setup-buildx v4, login v4, metadata v6.
- Watchtower: репозиторий `containrrr/watchtower` заархивирован 2025-12-17 (https://linuxhandbook.com/blog/watchtower-like-docker-tools/); поддерживаемый форк `nicholas-fedor/watchtower`, образ `nickfedor/watchtower`, релиз v1.22.3 от 2026-09-22.
- hyper-util `client/proxy/matcher.rs` (исходник 0.1.20): переменные `ALL_PROXY`, `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, localhost без `NO_PROXY` не обходится; `Proxy-Authorization` помечен `set_sensitive`.

## 3. Steps

Порядок: 3.1-3.3 (TLS-ядро), 3.4-3.7 (клиенты), 3.8-3.10 (hub), 3.11-3.12 (сборка), 3.13 (health), 3.14 (Linux), 3.15-3.18 (упаковка, CI, док). Проверка после каждого блока: `cargo test -p cctg --lib -- <модуль>`.

### 3.1 `crates/cctg/Cargo.toml`, `Cargo.lock`
- `[dependencies]`: `rustls = { version = "0.23", default-features = false, features = ["aws_lc_rs", "std"] }`, `tokio-rustls = { version = "0.26", default-features = false, features = ["aws_lc_rs"] }` (оба уже в lock через reqwest).
- `[dev-dependencies]`: `rcgen = { version = "0.14", default-features = false, features = ["aws_lc_rs", "pem"] }` для самоподписанных сертификатов в тестах. Новые записи lock: 26 пакетов (rcgen, yasna, time, x509-parser и их зависимости), только dev. Зачем: приёмка требует сертификат, сделанный в тесте; фикстура-сертификат протух бы.
- Проверка: `cargo build -p cctg --locked` после патча.

### 3.2 `crates/cctg/src/tls.rs` (новый), `lib.rs` (`pub mod tls;`)
- Константы `PIN_VAR = "CCTG_HUB_CERT_SHA256"`, `CERT_VAR = "CCTG_TLS_CERT"`, `KEY_VAR = "CCTG_TLS_KEY"`.
- `CertPin([u8; 32])`: `parse` (64 hex, `:`/пробелы, любой регистр, префикс до `=` отрезается: строка openssl), `of(der)` (sha256 через `::aws_lc_rs::digest`), сравнение `subtle::ConstantTimeEq`, Display `AB:CD:..`.
- `Pinned` verifier (см. Approach 2), провайдер `aws_lc_rs::default_provider()` явно (без глобального `install_default`).
- `host_of(addr)` (`host:port`, `[v6]:port`, порт u16), `is_loopback_addr` (`localhost` или loopback IP).
- `HubAddr { addr, tls: Option<(TlsConnector, ServerName)> }`: `plain(addr)` (без проверок, для тестов и loopback), `pinned(addr, pin)` (TLS 1.3 only, `ServerName` из host: IP или DNS), `connect()` (TCP, `set_nodelay`, рукопожатие; время ограничивает вызывающий), `is_tls()`, Debug без деталей.
- `Stream { Plain(TcpStream), Tls(Box<tokio_rustls::TlsStream<TcpStream>>) }` с делегирующими `AsyncRead`/`AsyncWrite`.
- `ReadTask`: задача чтения половины `tokio::io::split`, `abort` в `Drop`, `stop().await`. Зачем: половины split держат сокет открытым, пока жива любая; без этого отменённая задача соединения (остановленный `serve_agents`) оставляла ссылку агенту живой (2 теста `agent.rs` падали, мутация убита).
- `TlsFileError { Cert, Key, Pair }` фиксированным текстом; `Acceptor::from_files(cert, key) -> (Acceptor, CertPin)`, `Acceptor::new(chain, key)` (TLS 1.3, `with_single_cert`, он же отказывает несовпавшей паре), `accept(tcp)`. `Incoming { plain() | tls(acceptor) }::accept`.
- Тесты модуля: написание pin, host/loopback, TLS-эхо с верным и чужим pin, поддельный сервер с чужим ключом через `ResolvesServerCert` (обходит проверку пары, ловит пропуск проверки подписи), ошибки PEM без содержимого.

### 3.3 `crates/cctg/src/hub/ingress.rs`
- `bind` больше не предупреждает; новый `Listener { tcp, incoming }` с `From<TcpListener>` (plain: существующие вызовы в тестах не меняются), `Listener::tls`, `Listener::new(tcp, Option<Acceptor>)` (info «listening with TLS» или прежний warning о plain за loopback).
- `serve_agents`, `serve_hooks`, `serve_hooks_and_permissions` принимают `impl Into<Listener>`.
- `open(incoming, tcp, peer, limit)`: TLS-рукопожатие в задаче соединения (не в цикле accept: медленный клиент не держит остальных), ограничено `HANDSHAKE_TIMEOUT` (агенты) и `REQUEST_TIMEOUT` (хуки); ошибка и таймаут пишутся debug (сканеры портов).
- `agent_session`, `hook_request`, `respond`, `permission_request`, `gone` берут `Stream`; `tokio::io::split` вместо `into_split`; reader через `ReadTask`.
- Соединение, закрытое до первого байта: агент (`WireError::Closed` при пустой строке) и хук (`read_request` возвращает `Err(None)`) пишут debug «connection closed before its first byte» и не отвечают. Остальные ошибки `read_request` стали `Err(Some(status))`.
- `tests/ingress_logs.rs`: в начале пустые соединения к обоим слушателям: две строки debug, ни одного `WARN`.

### 3.4 `crates/cctg/src/device.rs`
- Поле `pin: Option<Result<CertPin, ConfigProblem>>` из `CCTG_HUB_CERT_SHA256`; новые `ConfigProblem::{BadPin, BadAddr, PlainRemote}` с фиксированным текстом.
- `DeviceConfig::hub(&self, addr) -> Result<HubAddr, ConfigProblem>`: pin → `HubAddr::pinned`; сломанный pin → `BadPin`; без pin loopback → plain; иначе `BadAddr`/`PlainRemote`.
- Тест `plain_only_to_loopback_and_tls_only_with_a_valid_pin`.

### 3.5 `crates/cctg/src/hook.rs`, `spool.rs`, `statusline.rs`
- `post`, `ask`, `deliver`, `spool::replay` принимают `&HubAddr`; `TcpStream::connect` заменён на `addr.connect()`.
- `run` и `permission` строят адрес через `config.hub(&config.hook_addr)`; ошибка конфигурации: одна строка stderr, событие не шлётся и не спулится (как с плохим секретом).
- `TLS_POST_TIMEOUT` 900 мс, `TLS_PROMPT_POST_TIMEOUT` 600 мс, `TLS_PERMISSION_CONNECT_TIMEOUT` 1000 мс; `post_timeout(event, tls)` публичная. Зачем: TLS добавляет круг до удалённого hub; stdin 300 + 900 ≤ 1200 мс, в бюджете `SessionEnd` 1.5 с (тест проверяет).
- Строка статуса: `config.hub(...)`, бюджет 80 мс не меняется (решение 10).
- Тесты модулей переведены на `HubAddr::plain(...)`.

### 3.6 `crates/cctg/src/agent.rs`
- `LinkConfig.addr: HubAddr`, `Replay.hook_addr: HubAddr`; `connect` через `config.addr.connect()` и `tokio::io::split`; типы `LinkRead`/`LinkWrite`; reader через `ReadTask`.
- `link_plan` возвращает `LinkPlan { secret, session_id, agent, hook }`; адрес без pin за loopback даёт `NoHub::NoConfig` с warning (тест).
- `Client.build` = `client::identity(client::SOURCE, || Some(file_hash))`; `worker.build` остаётся хешем файла для `update::Worker::plan`.

### 3.7 Остальные вызовы в тестах
`tests/ingress_logs.rs`, `status_e2e.rs`, `supervise_e2e.rs`, `files_e2e.rs`: адреса обёрнуты в `cctg::tls::HubAddr::plain(...)`.

### 3.8 `crates/cctg/src/hub/config.rs`
- `Config.tls: Option<TlsFiles { cert, key }>`; одна переменная из двух даёт `ConfigError::TlsPair`. Тест `tls_needs_both_files_or_none`.

### 3.9 `crates/cctg/src/hub/mod.rs`
- После bind: `Acceptor::from_files` при `config.tls` (ошибка называет переменную, не содержимое), `info!(sha256 = %pin, "TLS certificate loaded")` (pin не секрет, устройство берёт его из лога), `Listener::new` для обоих слушателей.
- `proxy_in_env()` и `info!("Bot API requests go through the proxy of the environment")` без значения.
- `healthy()` и `probe_target` для `cctg health` (3.13); лог старта через новый `client::short` (String).

### 3.10 Идентичность в hub: `hub/slots.rs`, `hub/status.rs`, `wire.rs`
- `slots.rs:999-1010`, `:4032-4035`: `client::short` теперь `String`.
- Новый тест `one_commit_on_two_systems_is_current_and_another_commit_is_warned_once`: hub и клиент с одним коммитом и разными хешами файлов не устаревшие; другой коммит: ровно одно предупреждение с обоими номерами (приёмка «разный коммит даёт одно предупреждение»).
- `status.rs` `NO_NEW_BUILD_NOTICE`: текст для обоих случаев (hub на этой машине: `cctg deploy`; иначе файл той же сборки из GitHub Releases, номер в предупреждении). Ключ `notify` остаётся `&'static str`.
- `wire.rs` doc `Client.build`: коммит, иначе sha256 exe.

### 3.11 `crates/cctg/build.rs` (новый)
- `CCTG_BUILD_ID` (1-64 символа `[A-Za-z0-9._-]`, иначе сборка падает), иначе git (`rev-parse HEAD`, `status --porcelain --untracked-files=normal -- :/crates :/Cargo.toml :/Cargo.lock`), иначе пусто. `cargo::rustc-env=CCTG_SOURCE=...`.
- `rerun-if-env-changed=CCTG_BUILD_ID`; `rerun-if-changed` только на существующие пути (`--git-path HEAD`, `packed-refs`, файл текущей ветки, `src`, `Cargo.toml`, `transcript/src`, корневые `Cargo.toml`/`Cargo.lock`): несуществующий путь заставил бы скрипт идти каждую сборку. Без git (Docker-контекст) не падает.

### 3.12 `crates/cctg/src/client.rs`, `main.rs`
- `SOURCE = env!("CCTG_SOURCE")`, `LONG_VERSION` (`0.1.0 (<source>)`), `identity(source, file_hash)`, `build_id_of(path)`, `own_build()` через `identity`, `short()` (8 символов + `-dirty`). Тесты: один коммит на двух «ОС», грязные и без git.
- `main.rs`: `#[command(version = cctg::client::LONG_VERSION)]` (`cctg deploy` проверяет только префикс `cctg `).
- `tests/update_e2e.rs`: `build_of` → `cctg::client::build_id_of`.

### 3.13 `cctg health`
- `main.rs`: `Command::Health` → `exit(if cctg::hub::healthy().await {0} else {1})`, тест разбора.
- `hub/mod.rs`: `healthy()` читает только `CCTG_AGENT_LISTEN`/`CCTG_HOOK_LISTEN` процесса (без токена и env-файла), `0.0.0.0`/`[::]` проверяет на loopback, 3 с на слушатель. Тест `probe_target`.

### 3.14 Linux (по `P/linux-baseline.log`)
- `hub/testdir.rs`: модуль тестов `#[cfg(all(test, windows))]` (его единственный тест Windows-only).
- `proctree.rs`: `ProcessEntry`, `ProcessTable`, `impl`, `use HashMap` под `#[cfg(any(windows, test))]`.
- `statusline.rs`: `git_bash_is_found_like_claude_code_finds_it` под `#[cfg(windows)]` (Git Bash ищется только на Windows).
- `tests/common/mod.rs`: `write_program(path, bytes)` (запись + `0o755` на Unix); `supervise_e2e.rs` (3 копии) и `update_e2e.rs` (2 копии) через него.
- `tests/slots_logs.rs`: хуки только после строки hub «waits for its SessionStart» в перехваченном логе; сообщение assert-а с логом. `tests/files_e2e.rs`: после `registered` старого агента ждать новую строку «agent bound to its session» (`bound_count`). Гипотеза: гонка каналов actor-а (`select!` без порядка), на Windows не воспроизводится (15 последовательных и 18 параллельных прогонов зелёные). Если CI на Linux всё ещё красный: по логу из assert-а искать настоящую причину, чинить в продукте и добавлять unit-тест.

### 3.15 `Dockerfile`, `.dockerignore`, `.gitignore`
- `Dockerfile` (multi-stage): `build` на `rust:${RUST_VERSION}-alpine` (`apk add musl-dev gcc`), `ARG CCTG_BUILD_ID`, `cargo build --release --locked -p cctg` с cache-mount, `/cctg --version`; stage `binary` (`FROM scratch`) для `--target binary --output`; runtime `alpine:3.21`, `adduser -u 10001`, `/data` (владелец cctg), `USER 10001`, `ENV CCTG_STATE_DIR=/data CCTG_AGENT_LISTEN=0.0.0.0:47291 CCTG_HOOK_LISTEN=0.0.0.0:47292`, `VOLUME`, `EXPOSE`, `HEALTHCHECK CMD ["cctg","health"]`, `ENTRYPOINT ["cctg"] CMD ["hub"]`.
- `.dockerignore`: allowlist (`*`, `!Cargo.toml`, `!Cargo.lock`, `!crates/`, минус `crates/**/target/`). Зачем: `.env` с токеном лежит в корне, в контекст сборки он не попадает никогда.
- `.gitignore`: `deploy/hub.env`, `deploy/tls/`.

### 3.16 `deploy/compose.yml`, `deploy/compose.host.yml`, `deploy/hub.env.example`
- `compose.yml`: `ghcr.io/pockerhead/cctg:latest`, `restart: unless-stopped`, `env_file: hub.env`, `CCTG_TLS_CERT`/`KEY` на `/tls`, тома `state:/data` и `./tls:/tls:ro`, порты 47291/47292, `extra_hosts: host.docker.internal:host-gateway`, `stop_grace_period: 30s`, healthcheck, метка watchtower, ротация логов json-file; сервис `updater` (`nickfedor/watchtower`, `--label-enable --cleanup --interval 600`) под профилем `autoupdate`.
- `compose.host.yml`: `network_mode: host`, `ports: !reset []`, `extra_hosts: !reset []` (Compose 2.24+).
- `hub.env.example`: четыре обязательные переменные, закомментированный `HTTPS_PROXY`.

### 3.17 `.github/workflows/ci.yml`, `release.yml`
- `ci.yml`: push в любую ветку и PR; `test` матрица `ubuntu-latest`/`windows-latest` (toolchain 1.95.0, rust-cache, fmt только на Linux, clippy `-D warnings`, `cargo test --workspace --locked --no-fail-fast`); `image`: buildx build без push, `CCTG_BUILD_ID=${{ github.sha }}`, `docker run --rm cctg:ci --version | grep <sha>`.
- `release.yml`: push в `main` и теги `v*`; `image` (GHCR: `:main`, `:sha-<полный>`, на теге `:<версия>` и `:latest`; тот же build stage выгружает Linux-бинарник на теге); `windows` (тег: `cargo build --release` с `CCTG_BUILD_ID`, проверка `--version`); `release` (`gh release create` с бинарниками и `SHA256SUMS`, `contents: write`). Секретов, кроме `GITHUB_TOKEN`, нет.

### 3.18 `docs/remote-hub.md` (новый), `CLAUDE.md`
- Док: схема, что где лежит, одноразовый сетап сервера (папка, `hub.env` chmod 600, сертификат openssl EC P-256 на 10 лет, `chown 10001` ключа, видимость пакета GHCR, `docker compose up -d`, pin из лога), прокси (две схемы, прокси демона Docker), `device.env` (pin: hex после `Fingerprint=` или вся строка в кавычках), обновление (тег, watchtower или pull), версии клиента и hub (как обновить удалённый клиент руками), локальный hub с TLS, проверка, ограничения. Без секретов.
- `CLAUDE.md`, раздел «Безопасность»: одна строка про TLS с pin и сборку = коммит.

## 4. Проверки implementer-а (критерии готовности)

1. `git apply --ignore-whitespace .../task035.patch` → `bash .../verify_hashes.sh`: все OK.
2. `cargo fmt --all -- --check`; `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`; `cargo test -j 1 --workspace --locked --no-fail-fast`: как в `P/workspace_test.txt`.
3. `python P/mutations/run_mutations.py C:/Users/user/dev/cctg`: 12 KILLED (меняет и возвращает файлы по одному; не запускать параллельно с другой сборкой).
4. `target/debug/cctg.exe --version` печатает `cctg 0.1.0 (<коммит>[-dirty])`.
5. Если Docker снова разрешат: `python P/docker/run_docker_e2e.py <корень> target/debug/cctg.exe <id>`, где клиент собран с тем же `CCTG_BUILD_ID=<id>`. Иначе явно: образ, compose, healthcheck и Linux-тесты проверяет первый прогон CI после push (пушит orchestrator, не implementer).

Соответствие приёмке: TLS e2e с настоящими процессами и самоподписанным сертификатом: `tests/tls_e2e.rs` (hub в контейнере: пункт 5 или CI); localhost без TLS: все прежние e2e (supervise, soak fake, stream, files...) остались plain и зелёные; Dockerfile/compose/healthcheck: файлы + `P/docker/` (не прогнано, раздел 0); CI и одноразовый сетап: `.github/workflows/*`, `docs/remote-hub.md`; секреты: `.dockerignore` allowlist, `tls_e2e`/`proxy_e2e` проверяют вывод hub на токен, секрет, пароль прокси, `run_docker_e2e.py` проверяет образ; сборка одного коммита не устаревшая и одно предупреждение для другого: `client::tests`, `slots::tests::one_commit_on_two_systems...`, `run_docker_e2e.py` шаги 3-4.

## 5. Risk areas

- **Linux не проверен после правок.** Docker остановлен пользователем. Первый зелёный Linux будет только в CI; `slots_logs`/`files_e2e` чинятся по гипотезе. Windows-раннер CI (сессия без интерактивной консоли) тоже ни разу не гонял тесты с консолью (`keys.rs`, `run_e2e`, `supervise_e2e` с Ctrl+Break): возможны падения, которых здесь не видно.
- **Образ не собирался.** `apk add musl-dev gcc` по документации aws-lc-rs; если aws-lc-sys на musl попросит `g++`/`cmake`/`perl`, добавить в build stage. Кэш `--mount=type=cache` в GHA не сохраняется между запусками (слои кэшируются через `type=gha`), первая сборка долгая.
- **Бюджеты хуков по TLS.** Три круга до hub (TCP, TLS 1.3, запрос): 600 мс для `UserPromptSubmit` выдерживает RTT около 180 мс. Строка статуса с 80 мс до далёкого hub не доедет: цифры в сообщении статуса удалённых устройств могут отсутствовать (решение 10, вопрос 6.2).
- **Ротация сертификата** меняет pin: все устройства замолкают до правки `device.env`. Hub пишет pin при каждом старте, док это описывает.
- **Интернет-шум.** Порты hub открыты миру: сканеры тратят места `MAX_AGENTS`/`MAX_HOOK_REQUESTS` на время таймаута рукопожатия (5 с / 2 с); ограничения скорости нет. Логи не засоряются (debug).
- **`build.rs` и пересборки.** Смена HEAD или файлов в `src` перезапускает скрипт; при неизменном выводе cargo может всё равно пересобрать крейт. `--path-format=absolute` нужен git 2.31+ (на CI и здесь есть). Старые hub/агенты (sha256) и новые (коммит) при переходе однократно считают друг друга устаревшими: одна волна «⬆️ Обновить» после выкатки.
- **Прокси.** Переменные читаются только из окружения процесса; `.env` hub их не передаёт (док предупреждает). `HTTP_PROXY` в окружении разработчика направит в прокси и тестовые запросы к fake Bot API на loopback (было и раньше; `proxy_e2e` чистит эти переменные только для своих процессов).
- **`compose.host.yml`** с `!reset` требует Compose 2.24+; сеть хоста отдаёт порты 47291/47292 хосту напрямую.
- **watchtower-форк** держит `/var/run/docker.sock` (root на хосте); профиль выключен по умолчанию.

## 6. Open questions

1. Linux-проверка до merge: разрешит ли пользователь снова Docker (тогда `P/docker/run_docker_e2e.py` и Linux-тесты здесь), или достаточно первого прогона CI после push? Без одного из двух приёмка «hub в контейнере» и «Dockerfile собирается» закрывается только файлами.
2. Строка статуса удалённых устройств: оставить 80 мс (цифры могут не доходить) или отдельной задачей возить цифры через уже открытую связь агента? В этой задаче оставлено 80 мс.
3. Видимость пакета GHCR: сделать образ публичным (репозиторий публичный) или логиниться на сервере токеном `read:packages`? Док описывает оба; решение за пользователем.
4. Мультиархитектура (arm64-сервер)? Сейчас только x86_64.

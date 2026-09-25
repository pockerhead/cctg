# TASK-035: remote-ready hub (client/server split, part 2)

Type: feature
Mode: full
Priority: high
Branch: feature/remote-hub
Domains: hub, hooks, channel

## Description
После TASK-034 hub не зависит от файлов машин. Подготовить его к удалённому серверу:
- Конфиг адреса прослушивания (сейчас localhost) для agent-линка и хук-ingress; клиентская сторона берёт адрес hub из `device.env` (уже есть) и умеет схему с TLS.
- Транспорт по публичной сети: TLS для agent-линка и хуков (например rustls: TCP+TLS для newline-JSON и HTTPS для хуков, либо один WebSocket поверх HTTPS; выбрать по простоте и зависимостям, записать решение). Секрет больше не ходит открытым текстом. Localhost без TLS остаётся для текущей схемы.
- Сборка сервера под Linux (статическая musl или обычная glibc в Docker multi-stage), Dockerfile и docker-compose (volume для state, `restart: unless-stopped`, env-файл с токеном), healthcheck.
- Выкатка без ssh от оркестратора: GitHub Actions собирает образ по тегу/пушу в registry (GHCR) и бинарники клиента в Releases; на сервере образ обновляется watchtower'ом или аналогом. Описать, что пользователь делает один раз на сервере.
- Разделение ролей в сборке: решить, остаётся ли один бинарник с подкомандами или сервер/клиент отдельными бинарями/фичами cargo (клиенту не нужен код бота и наоборот); обосновать.
- CI гоняет весь workspace (fmt, clippy, тесты) на Linux и Windows раннерах: тесты на Linux ещё ни разу не запускались, падения там чинятся в этой задаче; Windows-only тесты помечены cfg. Управление консолью на Linux не здесь, это TASK-044.
- Док: `docs/remote-hub.md` (развёртывание, TLS, обновление), без секретов.

## Dependencies
- blocked by TASK-034 — hard prerequisite

## Acceptance criteria
- [ ] hub в контейнере под Linux принимает агентов и хуки по TLS с другой машины (e2e с настоящими процессами и самоподписанным сертификатом в тесте)
- [ ] localhost-схема без TLS работает как раньше
- [ ] Dockerfile и compose собираются, образ стартует с env-файлом и volume, healthcheck зелёный
- [ ] CI-конфиг собирает образ и клиентские бинарники; описан одноразовый сетап сервера
- [ ] секреты не в логах, не в образе
- [ ] Existing tests pass

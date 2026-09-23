# TASK-010: cctg transport contracts — agent TCP and hook HTTP ingress

Type: feature
Mode: full
Priority: high
Branch: feature/transport-contracts
Domains: hub, channel, hooks

## Description
Во внутренних модулях `cctg` (`src/wire.rs`, ingress в `src/hub/`) определить версионированный newline-JSON протокол persistent-соединения agent↔hub по TCP (первым сообщением shared secret, ограничение длины строки, reconnect только на стороне агента) и отдельные serde-пейлоады плюс HTTP endpoint для одноразового POST от хука. Listener по умолчанию слушает loopback; не-loopback адрес требует явной настройки. Никакого отдельного `proto` крейта и никакого reconnect-цикла в хуке.

## Dependencies
- blocked by TASK-002 — hard prerequisite

## Acceptance criteria
- [ ] все варианты TCP-сообщений round-trip через serde; неизвестная версия или неизвестный вид сообщения дают контролируемую ошибку без паники
- [ ] неверный secret отвергается до обработки Register и не попадает в логи; строка сверх лимита закрывает соединение с ограниченной аллокацией
- [ ] reconnect с backoff и повторный Register на стороне агента доказаны тестом с рестартом hub
- [ ] hook endpoint принимает один аутентифицированный POST и отвечает быстро; повторная доставка того же события идемпотентна по ключу события
- [ ] loopback по умолчанию и явная не-loopback конфигурация покрыты тестами
- [ ] shared secret не логируется ни на одном пути, включая ошибки парсинга
- [ ] Existing tests pass

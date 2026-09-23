## Counter-example tested

`AgentMsg::Reply` может не содержать идентификатор сессии, а соединение агента после `/clear` может оставаться зарегистрированным под прежним session id; тогда hub способен отправить reply в тему прежней сессии или вообще не определить тему, и критерий «reply агента уходит в тему слота его сессии» не задаёт необходимую привязку после `/clear`.

## Primary-source investigation

- В wire-контракте `AgentMsg::Reply` действительно несёт только `text`, без session id (`crates/cctg/src/wire.rs:133-145`).
- Hub хранит текущую привязку сессии у самого соединения в `Conn.session` (`crates/cctg/src/hub/slots.rs:167-174`). При регистрации со stale env id метод `agent_session` ищет живую сессию того же `(host, claude_pid)` (`crates/cctg/src/hub/slots.rs:315-328`), а каждый `SessionStart` вызывает `follow_pid`, который заменяет `Conn.session` и переносит registry-привязку агента на новую сессию (`crates/cctg/src/hub/slots.rs:330-385`).
- Текущий feature-gap существует именно там, где его предполагает задача: для agent message обработана только `PermissionRequest`, а `Reply` попадает в ветку `agent message not routed yet` (`crates/cctg/src/hub/slots.rs:293-302`).
- Выполнена команда `cargo test -p cctg the_agent_follows_its_claude_process -- --nocapture`. Реальный итог: оба теста перестановок событий `/clear` прошли — `the_agent_follows_its_claude_process_through_clear ... ok`, `the_agent_follows_its_claude_process_when_the_new_start_comes_first ... ok`, `test result: ok. 2 passed; 0 failed`.

## Did it hold

Контрпример не подтвердился. Отсутствие session id внутри `Reply` не оставляет reply без определимой сессии: идентичность берётся из соединения, а соединение уже переносится на новую сессию по pid при `/clear` в обоих порядках hook-событий. При этом первичный код положительно подтверждает заявленное недостающее звено: `Reply` пока принимается, но не маршрутизируется.

## Verdict

PREMISE HOLDS — `crates/cctg/src/hub/slots.rs:293-302,315-385`; `cargo test -p cctg the_agent_follows_its_claude_process -- --nocapture` → `2 passed; 0 failed`

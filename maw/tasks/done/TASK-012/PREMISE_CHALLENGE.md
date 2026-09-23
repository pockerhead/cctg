## 1. Counter-example tested

The task is incomplete if the hub's primary hook wire contract cannot represent or accept the required `SubagentHandback` report and `SubagentStop` fields: the hook could deserialize and POST exactly the requested local data while the real endpoint rejects it or silently cannot carry it, so the payload-focused success predicate would be met without end-to-end report capture.

## 2. Primary-source investigation

The hub wire type already models `SubagentStop.agent_transcript_path` and `last_assistant_message`, and separately models `SubagentHandback { agent_id, message }` at `crates/cctg/src/wire.rs:394-404`. The only HTTP acceptance path decodes the body through that wire decoder and enqueues every successfully decoded `HookPost` at `crates/cctg/src/hub/ingress.rs:431-457`; there is no event-specific rejection in that path.

I ran:

`$env:CARGO_TARGET_DIR='C:\Users\user\dev\cctg\target'; cargo test --manifest-path 'C:\Users\user\dev\cctg\Cargo.toml' -p cctg wire::tests::hook -- --nocapture`

Real output included:

`test wire::tests::hook_posts_round_trip_and_keep_their_event_id ... ok`

`test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 160 filtered out`

That test iterates all hook samples, serializes each `HookPost`, calls `decode_hook`, and requires exact equality (`crates/cctg/src/wire.rs:711-725`). Its sample set includes both a populated `SubagentStop` and `SubagentHandback { message: "report" }` (`crates/cctg/src/wire.rs:519-532`).

## 3. Did it hold

No. The concrete counter-example did not hold: the required report-bearing variants are representable, they round-trip through the hub's actual decoder, and the HTTP ingress hands every successfully decoded event to its event channel. I found no primary-source evidence that this premise is mis-framed on the tested boundary.

## 4. Verdict

PREMISE HOLDS — `crates/cctg/src/wire.rs:394-404, 519-532, 711-725` plus the passing command output `test wire::tests::hook_posts_round_trip_and_keep_their_event_id ... ok` show that both required report forms are represented and accepted by the hub decoder

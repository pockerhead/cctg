## Counter-example tested

Two live sessions emit permission requests with the same five-letter `request_id`; because Telegram `callback_data` contains only the action and that id, the hub cannot unambiguously associate a callback with the originating session, so an allow/deny may be delivered to the wrong session or the acceptance predicate may pass while the real request remains open.

## Primary-source investigation

- `crates/cctg/src/channel.rs:414-420` confirms that the accepted identifier is only five lowercase letters excluding `l`, so the same valid value can occur in separate channel processes; `crates/cctg/src/wire.rs:125-145` confirms that the request body itself adds no session id.
- The agent-to-hub envelope retains the missing scope: `crates/cctg/src/hub/ingress.rs:52-65` attaches a hub-run-unique `conn` to every `PermissionRequest`, and `crates/cctg/src/hub/slots.rs:192-199,316-333` binds that connection to its session and outbound sender.
- The Telegram side also retains an independent correlation value outside `callback_data`: `crates/cctg/src/hub/updates.rs:46-50,130-148` carries the callback message's `message_id`. The executable fixture at `crates/cctg/src/hub/updates.rs:346-364` exercises `data = "allow:abcde"` and preserves `message_id = Some(10)` after the allowlist gate.
- Command actually run: `cargo test -p cctg hub::updates::tests::allowlisted_text_is_input_and_strangers_are_dropped -- --exact --nocapture`. Real output: `test hub::updates::tests::allowlisted_text_is_input_and_strangers_are_dropped ... ok` and `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 238 filtered out`.

## Did it hold

No. A duplicate five-letter id does not make the callback intrinsically ambiguous in the real system: the request is scoped by its agent connection, while the callback supplies the originating Telegram message id in addition to its compact data. The tested allowlist path also drops the outsider callback before producing `CallbackInput` (`crates/cctg/src/hub/updates.rs:138-148,346-364`). I found no positive primary-source evidence that the stated premise or success predicate fails under this counter-example.

## Verdict

PREMISE HOLDS — `crates/cctg/src/hub/ingress.rs:52-65` preserves per-agent request scope and `crates/cctg/src/hub/updates.rs:46-50,130-148` preserves callback message identity; the targeted callback/allowlist test passed with `1 passed; 0 failed`

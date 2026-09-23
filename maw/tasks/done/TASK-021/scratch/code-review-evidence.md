# Code-review evidence

- Disconfirmation case: a superseded duplicate connection cannot reply because
  `on_reply` requires `SessionEntry.agent == Some(conn)`. The case did not hold.
- Reproduction: the scratch crate `repro_reply` printed
  `late_reply_delivered_to_reused_topic=true`. It starts session A, keeps A's
  connection alive, ends A, reuses the slot for B, then sends a late reply from
  A. The reply is emitted to B's reused topic.
- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test -j 1 --workspace`: exit 0; cctg lib 233 passed, 1 ignored, and
  every integration/doc test passed.
- Build environment: `CARGO_PROFILE_DEV_DEBUG=0`, one build at a time, one
  `CARGO_TARGET_DIR` at `%TEMP%/cctg-t021-review-target`.

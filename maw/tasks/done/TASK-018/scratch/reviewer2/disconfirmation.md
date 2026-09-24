# plan-reviewer-2 disconfirmation (TASK-018)

Most concrete input that would make PLAN_V2 wrong (checked before evaluating anything else):

PLAN_V2 finding 6 says live mode "can delete a service message of someone else's topic" because
`soak.rs` forwards every real `forum_topic_edited` from getUpdates to the test hub. If that were
true, a live pass would delete a `forum_topic_edited` service message in one of the user's own topics.

Checked in code (`ws/crates/cctg/src/hub/slots.rs`, `Slots::on_control`):

```rust
if !self.options.can_delete
    || thread_id.and_then(|t| self.registry.slot_by_topic(t)).is_none()
{
    return;
}
self.hand_off(Work::Delete, Op::Delete { message_id });
```

The soak hub starts from a fresh `registry.json` in its own temp state dir, so `slot_by_topic` knows only
the three topics this run created. A foreign `forum_topic_edited` is dropped before any `deleteMessage`.
Result: the claim does NOT hold. What does hold from finding 6: the live poller acknowledges the bot's
shared update queue (accepted by OPEN_DECISIONS: the user's hub must be stopped, one consumer), the
harness records foreign service messages into its own "shown" list (harmless for deletes, but the
"left" assertion can then fail on a foreign topic), and synthetic `setMessageReaction` /
`answerCallbackQuery` with made-up ids are sent to the real chat (they must not be).

Second counter-example found on the way (not in PLAN_V2): `hook::tests::a_refused_kept_event_keeps_the_own_event_back`
answers `HTTP/1.1 503 ...` with bare LF line ends (a multi-line byte-string literal). `hook::parse_status`
requires `\r\n`, so the client fails with `PostError::BadResponse`, not with 503; the test passed for
the wrong reason (confirmed: with an exact assertion it failed `left: Err(BadResponse) right: Err(Status(503))`).
Rust turns a CRLF line end inside a string literal into LF, so a CRLF checkout does not help. Fixed with
explicit `\r\n` escapes and an exact `Err(PostError::Status(503))` assertion (mutation M20 restores the
old bytes and is killed).

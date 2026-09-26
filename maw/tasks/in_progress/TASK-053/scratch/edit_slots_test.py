"""Adds the TASK-053 slots unit test (one-off edit)."""
p = 'crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()
anchor = '''    #[tokio::test]
    async fn a_permission_request_consumed_after_session_end_is_dropped() {'''
assert s.count(anchor) == 1
test = '''    fn context(percent: u32) -> HookEvent {
        HookEvent::StatusLine {
            model: None,
            effort: None,
            context: Some(percent),
            five_hour: None,
            seven_day: None,
        }
    }

    fn compact(trigger: &str) -> HookEvent {
        HookEvent::PreCompact {
            trigger: Some(trigger.into()),
        }
    }

    fn compacted(pid: u32) -> HookEvent {
        HookEvent::SessionStart {
            source: Some("compact".into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        }
    }

    /// The first line of the status message of slot 0 at `at`.
    fn status_head(slots: &Slots, session: &str, at: Instant) -> String {
        let (text, _) = slots.status_view(SlotId(0), session, at);
        text.lines().next().unwrap_or_default().to_owned()
    }

    /// Topic lines about compactions, in send order.
    async fn compact_lines(fake: &Fake) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fake.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(100),
                    text,
                    notify: false,
                    ..
                } if text.starts_with("🗜") => Some(text),
                Op::Send { text, .. } if text.starts_with("🗜") => {
                    panic!("a compaction line with a sound: {text}")
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_compaction_shows_in_the_status_and_its_end_is_one_line() {
        let dir = TempDir::new("slots-compact");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, context(80)));
        slots.on_hook(&hook(A, compact("auto")));
        let now = Instant::now();
        assert_eq!(status_head(&slots, A, now), "🗜 Сжимаю контекст (авто)…");
        assert_eq!(
            status_head(&slots, A, now + Duration::from_secs(125)),
            "🗜 Сжимаю контекст (авто)… 2 мин"
        );
        // A repeat while it runs changes nothing; a status line sent while it
        // runs is still the old context.
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, context(83)));
        assert_eq!(status_head(&slots, A, now), "🗜 Сжимаю контекст (авто)…");
        assert_eq!(compact_lines(&fake).await, ["🗜 Сжимаю контекст (авто)…"]);

        slots.on_hook(&hook(A, compacted(10)));
        assert_eq!(status_head(&slots, A, Instant::now()), "💤 Ждёт вас");
        // The old percentage again is not the new one.
        slots.on_hook(&hook(A, context(83)));
        assert_eq!(compact_lines(&fake).await.len(), 1);
        slots.on_hook(&hook(A, context(12)));
        assert_eq!(
            compact_lines(&fake).await,
            [
                "🗜 Сжимаю контекст (авто)…",
                "🗜 Контекст сжат за 0 с: 83% → 12%"
            ]
        );
        assert!(slots.compactions.is_empty());
        // A second SessionStart(compact) with nothing running says nothing.
        slots.on_hook(&hook(A, compacted(10)));
        assert_eq!(compact_lines(&fake).await.len(), 2);

        // Ended, and no new percentage comes: the line goes without numbers.
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, compacted(10)));
        slots.check_compactions(Instant::now());
        assert!(!slots.compactions.is_empty(), "waits for the numbers");
        slots.check_compactions(Instant::now() + COMPACT_NUMBERS_WAIT);
        assert!(slots.compactions.is_empty());
        let lines = compact_lines(&fake).await;
        assert_eq!(
            lines[2..],
            ["🗜 Сжимаю контекст (вручную)…", "🗜 Контекст сжат за 0 с"]
        );

        // One that never ends is forgotten after COMPACT_MAX, with no line.
        slots.on_hook(&hook(A, compact("auto")));
        slots.check_compactions(Instant::now() + COMPACT_MAX);
        assert!(slots.compactions.is_empty());
        assert_eq!(status_head(&slots, A, Instant::now()), "💤 Ждёт вас");
        // And one cut by the session's end leaves no line either.
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        assert!(slots.compactions.is_empty());
        slots.on_hook(&hook(A, compacted(10)));
        let lines = compact_lines(&fake).await;
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(lines[4..].iter().all(|line| line.starts_with("🗜 Сжимаю")));
    }

    #[tokio::test]
    async fn a_compaction_without_a_status_line_is_told_at_once() {
        let dir = TempDir::new("slots-compact-plain");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        // No topic yet: nothing to show.
        slots.on_hook(&hook(A, compact("manual")));
        assert!(slots.compactions.is_empty());
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, HookEvent::PreCompact { trigger: None }));
        assert_eq!(status_head(&slots, A, Instant::now()), "🗜 Сжимаю контекст…");
        slots.on_hook(&hook(A, compacted(10)));
        assert!(slots.compactions.is_empty());
        assert_eq!(
            compact_lines(&fake).await,
            ["🗜 Сжимаю контекст…", "🗜 Контекст сжат за 0 с"]
        );
        // A nested run's compaction is not the slot's.
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ));
        slots.on_hook(&hook(B, compact("auto")));
        assert!(slots.compactions.is_empty());
    }

'''
s = s.replace(anchor, test + anchor)
open(p, 'w', encoding='utf-8', newline='').write(s)

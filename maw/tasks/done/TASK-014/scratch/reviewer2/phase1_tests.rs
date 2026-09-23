    // ---- TASK-014 review 2: lifecycle defects of the planner reference ----

    const CLOSED: &str = "Сессия завершилась";

    impl Rig {
        /// An agent that acknowledges verdicts (`Register::verdict_ack`).
        async fn agent_acking(&mut self, conn: u64, session: &str, claude_pid: Option<u32>) {
            self.agent_of(conn, session, claude_pid).await;
        }
    }

    /// The actor driven by direct calls over a Telegram that answers at once
    /// (answers are not fed back: there is no run loop).
    fn live_slots(dir: &TempDir, options: Options) -> (Arc<Fake>, Slots) {
        let store = RegistryStore::open(dir.path()).unwrap();
        let fake = Arc::new(Fake::default());
        let fast = BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        };
        let (scheduler, outbox) = Scheduler::new(fake.clone(), fast);
        tokio::spawn(scheduler.run());
        let slots = Slots::new(Registry::default(), store, outbox, options).0;
        (fake, slots)
    }

    fn edits_of(ops: &[Op], message: i64) -> Vec<(String, Option<serde_json::Value>)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id,
                    text,
                    reply_markup,
                } if *message_id == message => Some((text.clone(), reply_markup.clone())),
                _ => None,
            })
            .collect()
    }

    /// Five letters without `l`, distinct for every `n` below 25^5.
    fn request_id(mut n: usize) -> String {
        const LETTERS: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
        let mut id = String::new();
        for _ in 0..5 {
            id.push(LETTERS[n % LETTERS.len()] as char);
            n /= LETTERS.len();
        }
        id
    }

    #[tokio::test]
    async fn session_end_closes_its_open_prompt_and_a_late_press_does_nothing() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(11),
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_DEAD)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("late", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 1).await;
        assert!(
            verdicts(&received(&mut rig, 1).await).is_empty(),
            "no verdict after the end"
        );
        assert_eq!(answers(&ops), [Some(permissions::ANSWER_EXPIRED)]);
        assert_eq!(
            edits_of(&ops, message_id),
            [(CLOSED.to_owned(), Some(permissions::no_keyboard()))]
        );
    }

    #[tokio::test]
    async fn a_prompt_stays_in_its_slot_topic_when_clear_moves_the_slot_on() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "a")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (thread, _, old) = prompts(&ops).remove(0);
        assert_eq!(thread, 100);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        settled(&rig, |ops| {
            sent_to(ops, 100).contains(&"── session bbbbbbbb · new ──")
        })
        .await;
        // B, on the same agent (it follows its claude process), asks with the
        // same five letters.
        rig.agents.send(permission(1, "abcde", "b")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        let (thread, _, new) = prompts(&ops)[1].clone();
        assert_eq!(thread, 100, "the slot's topic");
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("old", Some(old), "allow:abcde"))
            .unwrap();
        rig.control
            .send(press("new", Some(new), "deny:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            verdicts(&received(&mut rig, 0).await),
            [("abcde".to_owned(), Behavior::Deny)]
        );
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_EXPIRED),
                Some(permissions::ANSWER_DENIED)
            ]
        );
        assert_eq!(
            edits_of(&ops, old),
            [(CLOSED.to_owned(), Some(permissions::no_keyboard()))]
        );
    }

    #[tokio::test]
    async fn an_ended_sessions_prompt_never_reaches_the_topic_its_slot_moved_on_to() {
        let dir = TempDir::new("slots-prompt-moved");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        connect(&mut slots, 1, A, Some(10));
        // A asks while its slot has no topic yet.
        slots.on_agent(permission(1, "abcde", "p"));
        slots.pump();
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "t", None);
        slots.pump();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                Op::Send {
                    permission: true,
                    ..
                }
            )),
            "{:?}",
            fake.ops()
        );
    }

    #[tokio::test]
    async fn a_verdict_lost_with_the_link_goes_again_to_the_reconnected_agent() {
        let mut rig = rig(Fake::default(), message_options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_acking(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(message_id), "allow:abcde"))
            .unwrap();
        assert_eq!(
            verdicts(&received(&mut rig, 0).await),
            [("abcde".to_owned(), Behavior::Allow)]
        );
        // The link drops before the agent read the verdict: no ack came back.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        rig.agent_acking(2, A, Some(10)).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Allow)],
            "the same verdict again"
        );
    }

    #[tokio::test]
    async fn a_verdict_never_reaches_another_session_on_the_same_pid() {
        let mut rig = rig(Fake::default(), message_options());
        // A hook without a pid: the registry cannot tie pid 10 to A.
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: None,
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        // An agent of another session reports the same claude pid (a reused
        // pid); its SessionStart has not arrived yet, so it waits unbound.
        rig.agent_of(7, "cccccccc-0000-4000-8000-000000000003", Some(10))
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("q", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 1).await;
        assert!(verdicts(&received(&mut rig, 1).await).is_empty());
        assert_eq!(answers(&ops), [Some(permissions::ANSWER_OFFLINE)]);
    }

    #[tokio::test]
    async fn the_waiting_icon_stays_while_another_prompt_of_the_session_is_open() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "1")).await.unwrap();
        rig.agents.send(permission(2, "bcdef", "2")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 2 && last_icon(ops, 101) == Some(ICON_WAITING)
        })
        .await;
        let shown = prompts(&ops);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(shown[0].2), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| edits_of(ops, shown[0].2).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            last_icon(&rig.fake.ops(), 101),
            Some(ICON_WAITING),
            "the second prompt is still open"
        );
        rig.control
            .send(press("q2", Some(shown[1].2), "allow:bcdef"))
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn a_prompt_telegram_refused_leaves_no_waiting_icon() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.fake
            .send_errors
            .lock()
            .unwrap()
            .push("Bad Request: message text is empty");
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        settled(&rig, |ops| {
            ops.iter().any(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(last_icon(&rig.fake.ops(), 101), Some(ICON_ALIVE));
    }

    #[tokio::test]
    async fn a_failed_decision_edit_is_tried_again_on_the_tick() {
        let options = Options {
            retry_every: Duration::from_millis(100),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, text, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.fake
            .message_edit_errors
            .lock()
            .unwrap()
            .push((500, "Internal Server Error"));
        rig.control
            .send(press("q1", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| !edits_of(ops, message_id).is_empty()).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let decided = (
            format!("{text}{}", permissions::ALLOWED_MARK),
            Some(permissions::no_keyboard()),
        );
        assert_eq!(
            edits_of(&rig.fake.ops(), message_id),
            [decided.clone(), decided],
            "one failure, one retry, then done"
        );
        assert_eq!(verdicts(&received(&mut rig, 1).await).len(), 1);
    }

    #[tokio::test]
    async fn a_full_prompt_book_expires_its_oldest_prompt_visibly() {
        let dir = TempDir::new("slots-prompt-book");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        for n in 0..permissions::MAX_PROMPTS {
            slots.on_agent(permission(1, &request_id(n), "p"));
        }
        // All shown (no pump: nothing else goes out).
        for key in 0..permissions::MAX_PROMPTS as u64 {
            slots.prompts.get_mut(key).unwrap().sent = true;
            slots.prompts.delivered(key, 5000 + key as i64);
        }
        slots.on_agent(permission(1, "zzzzz", "p"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = fake.ops();
        let old = edits_of(&ops, 5000);
        assert!(
            old.len() == 1 && old[0].1 == Some(permissions::no_keyboard()),
            "{ops:?}"
        );
        assert_eq!(ops.len(), 1, "{ops:?}");
    }

    #[tokio::test]
    async fn the_first_press_stays_the_answer_while_the_agent_is_away() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_NO_CHANNEL)).await;
        rig.control
            .send(press("q1", Some(message_id), "deny:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 1).await;
        // A second thought while the agent is still away changes nothing.
        rig.control
            .send(press("q2", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 2).await;
        rig.agent_of(2, A, Some(10)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        rig.control
            .send(press("q3", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 3).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Deny)],
            "the first choice, once"
        );
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_OFFLINE),
                Some(permissions::ANSWER_DECIDED),
                Some(permissions::ANSWER_DECIDED)
            ]
        );
    }


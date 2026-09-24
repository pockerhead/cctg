# Test edits of slots.rs for TASK-017 (applied once to ws/): existing tests
# whose OFFLINE_NOTICE behaviour the task replaces, plus the new tests.
import io, os
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = io.open(p, encoding='utf-8', newline='').read()

def rep(old, new):
    global s
    assert s.count(old) == 1, old
    s = s.replace(old, new)

def cut_fn(head, new):
    """Replaces the test that starts at `head` up to the next `#[tokio::test`/`#[test]`/`fn `."""
    global s
    start = s.index(head)
    rest = s[start + len(head):]
    ends = [i for i in (rest.find('\n    #[tokio::test'), rest.find('\n    #[test]'), rest.find('\n    /// '), rest.find('\n    fn ')) if i >= 0]
    end = start + len(head) + min(ends) + 1
    s = s[:start] + new + s[end:]

cut_fn("""    #[tokio::test]
    async fn a_message_nobody_can_take_gets_one_notice_each() {""", """    #[tokio::test]
    async fn a_message_nobody_can_take_now_is_kept_and_told_once() {
        let options = Options {
            notice_every: Duration::ZERO,
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // A running session without an agent: kept, told once per period.
        rig.control.send(say(Some(100), 1, Some("one"))).unwrap();
        rig.control.send(say(Some(100), 2, Some("two"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 1).await;
        // A photo: only text is forwarded.
        rig.control.send(say(Some(100), 3, None)).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 2).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            sent_to(&rig.fake.ops(), 100),
            [buffer::QUEUED_NOTICE, TEXT_ONLY_NOTICE]
        );
        // The agent comes: both messages, in order, once; no Resume button.
        rig.agent_of(1, A, Some(10)).await;
        let got = received(&mut rig, 0).await;
        assert_eq!(contents(&got), ["one", "two"]);
        // An agent whose link queue is gone: the message waits, no notice.
        rig._to_agent.clear();
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(sent_to(&rig.fake.ops(), 100).len(), 2);
    }

    fn contents(got: &[HubMsg]) -> Vec<&str> {
        got.iter()
            .filter_map(|msg| match msg {
                HubMsg::Inbound { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect()
    }

""")

rep("""        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100) == [OFFLINE_NOTICE]).await;
        assert!(received(&mut rig, 0).await.is_empty());""", """        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        let resume = buffer::resume_text(A);
        settled(&rig, |ops| sent_to(ops, 100) == [resume.as_str()]).await;
        assert!(received(&mut rig, 0).await.is_empty());""")

rep("""        rig.hook(start(B, 11)).await; // slot 101, no agent: notices""",
    """        rig.hook(start(B, 11)).await; // slot 101, no agent: kept""")
rep("""        // Far more replies than the scheduler queue (1024) and the cap, plus
        // a burst to a slot without an agent (one notice): every send is""",
    """        // Far more replies than the scheduler queue (1024) and the cap, plus
        // a burst to a slot without an agent (kept, two notices): every send is""")

cut_fn("""    #[tokio::test(start_paused = true)]
    async fn a_burst_to_a_dead_slot_gets_one_notice_a_minute() {""", """    #[tokio::test(start_paused = true)]
    async fn a_burst_of_photos_gets_one_notice_a_minute() {
        let dir = TempDir::new("slots-notice");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.registry.topic_created(SlotId(1), 101, "b", None);
        for i in 0..10 {
            slots.on_control(say(Some(100), i, None));
        }
        assert_eq!(slots.queued_messages, 1, "one text-only notice for the burst");
        for i in 10..15 {
            slots.on_control(say(Some(101), i, None));
        }
        assert_eq!(slots.queued_messages, 2, "one notice for the other slot");
        tokio::time::advance(Duration::from_secs(59)).await;
        slots.on_control(say(Some(100), 20, None));
        assert_eq!(slots.queued_messages, 2, "still inside the minute");
        tokio::time::advance(Duration::from_secs(2)).await;
        slots.on_control(say(Some(100), 21, None));
        assert_eq!(slots.queued_messages, 3, "a minute later: one more");
    }

    fn buffered(slots: &Slots, slot: usize) -> Vec<i64> {
        slots.registry.slots[slot]
            .buffer
            .messages
            .iter()
            .map(|parked| parked.message_id)
            .collect()
    }

    fn end(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(pid),
            },
        )
    }

    /// Like [`connect`], with a link queue that holds `capacity` messages;
    /// returns the agent's end.
    fn connect_queue(
        slots: &mut Slots,
        conn: u64,
        session: &str,
        claude_pid: Option<u32>,
        capacity: usize,
    ) -> mpsc::Receiver<HubMsg> {
        let (to_agent, from_hub) = mpsc::channel(capacity);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
                verdict_ack: false,
                transcript_reads: false,
            },
            to_agent,
        });
        from_hub
    }

    fn drain(from_hub: &mut mpsc::Receiver<HubMsg>) -> Vec<i64> {
        let mut ids = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            if let HubMsg::Inbound { meta, .. } = msg {
                ids.push(meta["message_id"].parse().unwrap());
            }
        }
        ids
    }

    #[tokio::test]
    async fn the_51st_message_drops_the_oldest_and_warns_once_per_period() {
        let dir = TempDir::new("slots-buffer-cap");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        for i in 0..60 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        assert_eq!(buffered(&slots, 0), (10..60).collect::<Vec<_>>());
        assert_eq!(slots.queued_messages, 1, "one overflow notice, no queued one");
        slots.pump();
        assert_eq!(slots.queued_messages, 2, "and one Resume message");
        slots.pump();
        slots.on_control(say(Some(100), 60, Some("x")));
        assert_eq!(slots.queued_messages, 2, "nothing more in the same period");

        // A new session takes the slot: all 50 go in order, once.
        slots.on_hook(&start(B, 11));
        let mut from_hub = connect_queue(&mut slots, 1, B, Some(11), 64);
        slots.pump();
        assert_eq!(drain(&mut from_hub), (11..61).collect::<Vec<_>>());
        assert!(slots.registry.slots[0].buffer.is_idle());
        slots.pump();
        assert!(drain(&mut from_hub).is_empty(), "never twice");

        // The next dead period warns again.
        slots.on_hook(&end(B, 11));
        let before = slots.queued_messages;
        for i in 100..151 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        assert_eq!(slots.queued_messages, before + 1);
    }

    #[tokio::test]
    async fn a_full_link_queue_keeps_the_rest_in_order_for_the_next_try() {
        let dir = TempDir::new("slots-buffer-queue");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        for i in 0..6 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(10), 4);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [0, 1, 2, 3]);
        // A new message queues behind the kept ones, never ahead.
        slots.on_control(say(Some(100), 6, Some("x")));
        assert_eq!(drain(&mut from_hub), [4, 5, 6]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn revival_by_resume_new_session_or_clear_delivers_once_in_order() {
        for how in ["resume", "new", "clear"] {
            let dir = TempDir::new("slots-revival");
            let mut slots = stalled_slots(&dir, message_options());
            slots.on_hook(&start(A, 10));
            slots.registry.topic_created(SlotId(0), 100, "a", None);
            let mut next = match how {
                // `/clear` with no agent on line: the slot never looked dead.
                "clear" => {
                    for i in 0..3 {
                        slots.on_control(say(Some(100), i, Some("x")));
                    }
                    slots.on_hook(&hook(
                        A,
                        HookEvent::SessionEnd {
                            reason: Some("clear".into()),
                            claude_pid: Some(10),
                        },
                    ));
                    slots.on_hook(&hook(
                        B,
                        HookEvent::SessionStart {
                            source: Some("clear".into()),
                            claude_pid: Some(10),
                            parent_claude_pid: None,
                        },
                    ));
                    // The channel server keeps the pre-clear id (TASK-013).
                    connect_queue(&mut slots, 1, A, Some(10), 64)
                }
                _ => {
                    slots.on_hook(&end(A, 10));
                    for i in 0..3 {
                        slots.on_control(say(Some(100), i, Some("x")));
                    }
                    slots.pump();
                    let session = if how == "resume" { A } else { B };
                    slots.on_hook(&hook(
                        session,
                        HookEvent::SessionStart {
                            source: Some(if how == "resume" { "resume" } else { "startup" }.into()),
                            claude_pid: Some(11),
                            parent_claude_pid: None,
                        },
                    ));
                    slots.pump();
                    // The agent registers a moment after SessionStart.
                    assert_eq!(buffered(&slots, 0), [0, 1, 2], "{how}: waits for the agent");
                    connect_queue(&mut slots, 1, session, Some(11), 64)
                }
            };
            slots.pump();
            assert_eq!(drain(&mut next), [0, 1, 2], "{how}");
            slots.pump();
            slots.on_tick();
            slots.pump();
            assert!(drain(&mut next).is_empty(), "{how}: once");
            assert!(slots.registry.slots[0].buffer.is_idle(), "{how}");
            assert_eq!(slots.registry.slots.len(), 1, "{how}: same slot");
        }
    }

    #[tokio::test]
    async fn nested_runs_and_subagents_never_revive_a_slot() {
        let dir = TempDir::new("slots-no-revival");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        for i in 0..3 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        // A nested `claude -p --resume A` inside B, with its agent linked.
        slots.on_hook(&hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(11),
            },
        ));
        let mut nested_resume = connect_queue(&mut slots, 1, A, Some(20), 64);
        // A nested run of B and its agent.
        let nested = "cccccccc-0000-4000-8000-000000000003";
        slots.on_hook(&hook(
            nested,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(21),
                parent_claude_pid: Some(11),
            },
        ));
        let mut nested_agent = connect_queue(&mut slots, 2, nested, Some(21), 64);
        // Subagent hooks of the ended session.
        slots.on_hook(&hook(
            A,
            HookEvent::SubagentStart {
                agent_id: "a1b2c3d4e5f607182".into(),
                agent_type: "Explore".into(),
            },
        ));
        slots.pump();
        slots.on_tick();
        slots.pump();
        assert!(drain(&mut nested_resume).is_empty());
        assert!(drain(&mut nested_agent).is_empty());
        assert_eq!(buffered(&slots, 0), [0, 1, 2]);
        assert_eq!(slots.registry.state(SlotId(0)), SlotState::Dead);
    }

""")

cut_fn("""    #[tokio::test]
    async fn the_backlog_of_messages_for_telegram_is_capped() {""", """    #[tokio::test]
    async fn the_backlog_of_messages_for_telegram_is_capped() {
        let dir = TempDir::new("slots-cap");
        let options = Options {
            notice_every: Duration::ZERO,
            ..message_options()
        };
        let mut slots = stalled_slots(&dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "t", None);
        // Photos: each asks for a text-only notice.
        for i in 0..MAX_QUEUED_MESSAGES as i64 + 50 {
            slots.on_control(say(Some(100), i, None));
        }
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
        assert!(slots.overflow_warned);
        // An answer frees one place and the next notice takes it.
        slots.on_done(Done::Message(None));
        slots.on_control(say(Some(100), 1000, None));
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
    }

""")

# Rig-level tests: the Resume button, its press and its end; a restart.
anchor = """    fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {"""
rep(anchor, """    fn resume_sends(ops: &[Op]) -> Vec<(i64, Option<serde_json::Value>)> {
        let mut message_id = 1000;
        let mut found = Vec::new();
        for op in ops {
            if let Op::Send {
                text, reply_markup, ..
            } = op
            {
                if text.starts_with("Сессия ") && text.contains("claude --resume") {
                    found.push((message_id, reply_markup.clone()));
                }
                message_id += 1;
            }
        }
        found
    }

    #[tokio::test]
    async fn a_dead_slot_shows_one_resume_button_that_records_the_wish() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(end(A, 10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        for i in 1..=3 {
            rig.control.send(say(Some(100), i, Some("x"))).unwrap();
        }
        let ops = settled(&rig, |ops| resume_sends(ops).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = rig.fake.ops().into_iter().skip(ops.len()).collect::<Vec<_>>();
        assert!(resume_sends(&ops).is_empty(), "one button per period");
        let (message, markup) = resume_sends(&rig.fake.ops())[0].clone();
        let data = markup.unwrap()["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(data, format!("resume:{A}"));
        assert!(data.len() <= permissions::MAX_CALLBACK_DATA);
        assert!(
            !rig.fake.ops().iter().any(|op| matches!(op, Op::Delete { .. })),
            "the topic stays as it is"
        );

        rig.control.send(press("q1", Some(message), &data)).unwrap();
        rig.control
            .send(press("q2", Some(message), "resume:0000"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            answers(&ops),
            [
                Some(buffer::ANSWER_UNAVAILABLE),
                Some(permissions::ANSWER_EXPIRED)
            ]
        );
        let saved = || std::fs::read_to_string(rig.dir.path().join("registry.json")).unwrap();
        let wait = async {
            while !saved().contains("\\"resume_asked\\": true") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, wait).await.expect("wish saved");

        // The session comes back: the messages go, the button goes.
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(12),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.agent_of(2, A, Some(12)).await;
        let ops = settled(&rig, |ops| !edits_of(ops, message).is_empty()).await;
        assert_eq!(
            edits_of(&ops, message),
            [(
                buffer::RESUMED_TEXT.to_owned(),
                Some(permissions::no_keyboard())
            )]
        );
        let got = received(&mut rig, 1).await;
        assert_eq!(contents(&got), ["x"; 3]);
        rig.control.send(press("q3", Some(message), &data)).unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 3).await;
        assert_eq!(answers(&ops)[2], Some(buffer::ANSWER_ALIVE));
    }

    #[tokio::test]
    async fn kept_messages_survive_a_restart_and_go_out_once() {
        // The previous run: A ended, three messages kept, the button out.
        let dir = TempDir::new("slots-buffer-restart");
        {
            let mut slots = stalled_slots(&dir, message_options());
            slots.on_hook(&start(A, 10));
            slots.registry.topic_created(SlotId(0), 100, "a", None);
            slots.on_hook(&end(A, 10));
            for i in 1..=3 {
                slots.on_control(say(Some(100), i, Some(&format!("m{i}"))));
            }
            slots.offer_resume();
            if let Some(note) = slots.registry.slots[0].buffer.resume.as_mut() {
                note.message_id = Some(900);
            }
            let store = RegistryStore::open(dir.path()).unwrap();
            store.save(&RegistryStore::encode(&slots.registry)).unwrap();
        }
        let path = dir.path().join("registry.json");
        let mut rig = rig_in(Fake::default(), message_options(), dir);
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(12),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.agent_of(1, A, Some(12)).await;
        let got = received(&mut rig, 0).await;
        assert_eq!(contents(&got), ["m1", "m2", "m3"]);
        let ops = settled(&rig, |ops| !edits_of(ops, 900).is_empty()).await;
        assert!(resume_sends(&ops).is_empty(), "the button is not offered again");
        // What the next restart would load: nothing left to send.
        let wait = async {
            loop {
                let registry: Registry =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if registry.slots[0].buffer.is_idle() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, wait).await.expect("emptied buffer saved");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(received(&mut rig, 0).await.is_empty());
    }

""" + anchor)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')

# Second pass (run after the first): TASK-014 foreign-press test. `resume:x`
# is now a Resume button of a session the hub does not know.
s = io.open(p, encoding='utf-8', newline='').read()
rep('''            press("q5", Some(message_id), "resume:x"),         // another button''',
    '''            press("q5", Some(message_id), "resume:x"),         // Resume of no known session''')
rep('''        assert_eq!(answers(&ops), [expired, expired, expired, None, None]);''',
    '''        assert_eq!(answers(&ops), [expired, expired, expired, None, expired]);''')
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok 2')

"""Fixer self-check edit: new TASK-017 tests and the two timing tests."""
p = 'C:/Users/user/dev/cctg/crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, (old, s.count(old))
    s = s.replace(old, new)


anchor = '''    fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {'''
new_tests = '''    /// Swaps the dispatch task of a directly driven actor for a channel the
    /// test reads; Telegram answers only what the test feeds back.
    fn capture_dispatch(slots: &mut Slots) -> mpsc::UnboundedReceiver<(Work, Op)> {
        let (dispatch, work) = mpsc::unbounded_channel();
        slots.dispatch = dispatch;
        work
    }

    /// The numbers of the Resume sends and the message edits handed out
    /// since the last call.
    fn handed(work: &mut mpsc::UnboundedReceiver<(Work, Op)>) -> (Vec<u64>, Vec<(i64, String)>) {
        let (mut sends, mut edits) = (Vec::new(), Vec::new());
        while let Ok((job, op)) = work.try_recv() {
            match (job, op) {
                (Work::Resume { number, .. }, _) => sends.push(number),
                (
                    _,
                    Op::Edit {
                        message_id, text, ..
                    },
                ) => edits.push((message_id, text)),
                _ => {}
            }
        }
        (sends, edits)
    }

    /// Telegram took Resume send `number` of slot 0 as `message_id`.
    fn resume_sent(slots: &mut Slots, number: u64, message_id: i64) {
        slots.on_done(Done::Resume {
            slot: SlotId(0),
            number,
            delivery: Some(Ok(Outcome::Sent(Message {
                message_id,
                ..Message::default()
            }))),
        });
    }

    fn resumed(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    /// A ended with one message kept and its Resume send out; returns the
    /// number of that send.
    fn dead_slot_with_a_button(slots: &mut Slots, work: &mut mpsc::UnboundedReceiver<(Work, Op)>) -> u64 {
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        slots.on_control(say(Some(100), 1, Some("m1")));
        slots.pump();
        let (sends, _) = handed(work);
        assert_eq!(sends.len(), 1, "one Resume send");
        sends[0]
    }

    #[tokio::test]
    async fn a_resume_message_answered_after_its_period_ended_loses_its_button() {
        let dir = TempDir::new("slots-resume-late");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        let first = dead_slot_with_a_button(&mut slots, &mut work);
        // A comes back before Telegram answered the send: the period ends.
        slots.on_hook(&resumed(A, 12));
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(12), 8);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [1]);
        assert!(slots.registry.slots[0].buffer.is_idle());
        assert!(handed(&mut work).1.is_empty(), "no message id to edit yet");
        resume_sent(&mut slots, first, 901);
        assert_eq!(
            handed(&mut work).1,
            [(901, buffer::RESUMED_TEXT.to_owned())]
        );
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn a_late_answer_of_an_earlier_period_never_takes_the_new_button() {
        let dir = TempDir::new("slots-resume-race");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        let first = dead_slot_with_a_button(&mut slots, &mut work);
        slots.on_hook(&resumed(A, 12));
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(12), 8);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [1]);
        // A ends again and a message comes: a second button goes out while
        // Telegram still has not answered the first.
        slots.on_hook(&end(A, 12));
        slots.on_control(say(Some(100), 2, Some("m2")));
        slots.pump();
        let (sends, _) = handed(&mut work);
        assert_eq!(sends.len(), 1);
        let second = sends[0];
        assert_ne!(first, second);

        resume_sent(&mut slots, first, 901);
        assert_eq!(
            handed(&mut work).1,
            [(901, buffer::RESUMED_TEXT.to_owned())],
            "the first period is over"
        );
        let note = |slots: &Slots| slots.registry.slots[0].buffer.resume.clone().unwrap();
        assert_eq!(note(&slots).message_id, None);
        resume_sent(&mut slots, second, 902);
        assert!(handed(&mut work).1.is_empty(), "the live button stays");
        assert_eq!(note(&slots).message_id, Some(902));
        assert_eq!(buffered(&slots, 0), [2]);
    }

    #[tokio::test]
    async fn a_resume_press_still_counts_after_a_later_session_ended_in_the_slot() {
        let dir = TempDir::new("slots-resume-later-end");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        dead_slot_with_a_button(&mut slots, &mut work);
        // A top-level session without a channel takes the slot and ends.
        slots.on_hook(&start(B, 11));
        assert_eq!(slots.registry.slots[0].current_session.as_deref(), Some(B));
        slots.on_hook(&end(B, 11));
        slots.pump();
        assert!(handed(&mut work).0.is_empty(), "one button per period");
        assert_eq!(slots.press_resume(A), buffer::ANSWER_UNAVAILABLE);
        assert!(slots.registry.slots[0].buffer.resume_asked);
        assert_eq!(buffered(&slots, 0), [1]);
    }

'''
rep(anchor, new_tests + anchor)

rep('''        // Saved to disk and routed for /brief.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let saved = RegistryStore::open(rig.dir.path()).unwrap().load().unwrap();
        assert_eq!(saved.slots.len(), 1);''', '''        // Saved to disk and routed for /brief.
        let store = RegistryStore::open(rig.dir.path()).unwrap();
        let saved_b = async {
            loop {
                if let Ok(saved) = store.load()
                    && saved.slots.first().and_then(|slot| slot.current_session.as_deref())
                        == Some(B)
                {
                    return saved;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let saved = tokio::time::timeout(WAIT, saved_b)
            .await
            .expect("B saved");
        assert_eq!(saved.slots.len(), 1);''')

rep('''    /// Feeds every finished background job to the directly driven actor
    /// until none comes for a while.
    async fn drain_done(slots: &mut Slots, done: &mut mpsc::UnboundedReceiver<Done>) {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut any = false;
            while let Ok(finished) = done.try_recv() {
                slots.on_done(finished);
                any = true;
            }
            if !any {
                return;
            }
        }
    }
''', '''    /// Feeds finished background jobs to the directly driven actor until
    /// `ready` holds.
    async fn drain_until(
        slots: &mut Slots,
        done: &mut mpsc::UnboundedReceiver<Done>,
        ready: impl Fn(&Slots) -> bool,
    ) {
        let reached = async {
            while !ready(slots) {
                let finished = done.recv().await.expect("done channel open");
                slots.on_done(finished);
            }
        };
        tokio::time::timeout(WAIT, reached)
            .await
            .expect("background jobs finished in time");
    }
''')
rep('''        slots.on_hook(&sub_start(A, S1, "Explore"));
        drain_done(&mut slots, &mut done).await;
        assert!(slots.registry.subagents.contains_key(S1));''', '''        slots.on_hook(&sub_start(A, S1, "Explore"));
        drain_until(&mut slots, &mut done, |slots| {
            slots.registry.subagents.contains_key(S1)
        })
        .await;''')
rep('''        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Late."));
        drain_done(&mut slots, &mut done).await;
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(format!("↳ Explore {S1}: one\\nLate.").as_str())
        );''', '''        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Late."));
        let late = format!("↳ Explore {S1}: one\\nLate.");
        drain_until(&mut slots, &mut done, |slots| {
            slots.registry.subagents[S1].block.pending.as_deref() == Some(late.as_str())
        })
        .await;''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("ok")

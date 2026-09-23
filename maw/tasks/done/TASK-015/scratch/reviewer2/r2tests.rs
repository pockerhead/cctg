
    /// A registry with A's topic (100) and the subagents `agents` of A
    /// confirmed; `legacy`: saved like TASK-011 code did, without a block.
    fn saved_with_subagents(dir: &TempDir, agents: &[&str], legacy: bool) {
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, 10));
        for job in registry.topic_work(&Icons::default(), true) {
            if let TopicJob::Create { slot, name, icon } = job {
                registry.topic_created(slot, 100, &name, icon.as_deref());
            }
        }
        for agent in agents {
            registry.confirm_subagent(agent, A, format!("↳ Explore {agent}"));
        }
        let mut json: serde_json::Value =
            serde_json::from_slice(&RegistryStore::encode(&registry)).unwrap();
        if legacy {
            for entry in json["subagents"].as_object_mut().unwrap().values_mut() {
                entry.as_object_mut().unwrap().remove("block");
            }
        }
        store.save(&serde_json::to_vec(&json).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_legacy_subagent_record_never_becomes_a_block() {
        // TASK-011 recorded every typed hook, internal agents included.
        let dir = TempDir::new("slots-legacy-ghost");
        saved_with_subagents(&dir, &[INTERNAL], true);
        let rig = rig_in(Fake::default(), subagent_options(), dir);
        let gone = rig.dir.path().join("agent-missing.jsonl");
        rig.hook(sub_stop(A, INTERNAL, "my-agent", &gone, "Ghost."))
            .await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. } | Op::Edit { .. })),
            0,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_first_send_cut_off_by_a_restart_stays_unsent() {
        // S1's send was out when the hub stopped: Telegram may show it.
        let dir = TempDir::new("slots-unknown-send");
        {
            let store = RegistryStore::open(dir.path()).unwrap();
            saved_with_subagents(&dir, &[S1], false);
            let mut registry = store.load().unwrap();
            assert_eq!(registry.block_work(usize::MAX).len(), 1);
            store.save(&RegistryStore::encode(&registry)).unwrap();
        }
        let rig = rig_in(Fake::default(), subagent_options(), dir);
        let gone = rig.dir.path().join("agent-missing.jsonl");
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Late.")).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. } | Op::Edit { .. })),
            0,
            "at most once: {ops:?}"
        );
        // The tombstone is on disk: no text waits, the send is marked.
        let saved = RegistryStore::open(rig.dir.path())
            .unwrap()
            .load()
            .unwrap();
        let block = &saved.subagents[S1].block;
        assert!(block.sending && block.pending.is_none() && !block.running);
        assert_eq!(block.message_id, None);
    }

    #[tokio::test]
    async fn a_first_send_with_an_unclear_answer_is_not_sent_again() {
        let dir = TempDir::new("slots-unclear-send");
        let parent = dir.path().join("parent.jsonl");
        std::fs::write(&parent, call_line("t1", "one") + &result_line("t1", S1)).unwrap();
        let fake = Fake {
            unclear_sends: Mutex::new(1),
            ..Fake::default()
        };
        let options = Options {
            retry_every: Duration::from_millis(50),
            ..subagent_options()
        };
        let rig = rig(fake, options);
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.ops_after(2).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let gone = dir.path().join("agent-missing.jsonl");
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Done.")).await;
        // Longer than the scheduler's 1 s gap between sends.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. })),
            1,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_first_send_is_tried_again() {
        let dir = TempDir::new("slots-refused-send");
        let parent = dir.path().join("parent.jsonl");
        std::fs::write(&parent, call_line("t1", "one") + &result_line("t1", S1)).unwrap();
        let fake = Fake {
            send_errors: Mutex::new(vec!["Bad Request: not enough rights"]),
            ..Fake::default()
        };
        let options = Options {
            retry_every: Duration::from_millis(50),
            ..subagent_options()
        };
        let rig = rig(fake, options);
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Send { .. })) == 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(rig.fake.ops().len(), ops.len());
    }

    #[tokio::test]
    async fn a_nested_answer_survives_a_restart_before_its_end() {
        let first = rig(Fake::default(), options());
        first.hook(start(A, 10)).await;
        first.ops_after(1).await;
        first
            .hook(hook(
                B,
                HookEvent::SessionStart {
                    source: Some("startup".into()),
                    claude_pid: Some(20),
                    parent_claude_pid: Some(10),
                },
            ))
            .await;
        first.ops_after(2).await;
        first.hook(stop(B, Some("nested answer"))).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let Rig { dir, .. } = first;
        let rig = rig_in(Fake::default(), options(), dir);
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(20),
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Edit { .. })) == 1
        })
        .await;
        let edits: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Edit { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(edits, ["⇣ nested bbbbbbbb\nnested answer"]);
    }

    #[tokio::test]
    async fn a_huge_call_description_still_fits_one_message() {
        let dir = TempDir::new("slots-huge-header");
        let parent = dir.path().join("parent.jsonl");
        let description = "описание ".repeat(2000);
        std::fs::write(
            &parent,
            call_line("t1", &description) + &result_line("t1", S1),
        )
        .unwrap();
        let rig = rig(Fake::default(), subagent_options());
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.ops_after(2).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| {
                matches!(op, Op::Edit { text, .. }
                    if text.ends_with(crate::hub::registry::BLOCK_LOST))
            })
        })
        .await;
        for op in &ops {
            if let Op::Send { text, .. } | Op::Edit { text, .. } = op {
                assert!(transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT);
            }
        }
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. })),
            1,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_block_confirmed_after_its_session_ended_is_marked_lost() {
        let dir = TempDir::new("slots-late-confirm");
        let parent = dir.path().join("parent.jsonl");
        std::fs::write(&parent, call_line("t1", "late")).unwrap();
        let rig = rig(Fake::default(), subagent_options());
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        std::io::Write::write_all(&mut file, result_line("t1", S1).as_bytes()).unwrap();
        drop(file);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let texts = shown(&rig.fake.ops());
        assert_eq!(
            block_of(&texts, S1)[0].1,
            &format!("↳ Explore {S1}: late\n{}", crate::hub::registry::BLOCK_LOST)
        );
    }

    #[tokio::test]
    async fn block_messages_in_flight_are_capped() {
        let dir = TempDir::new("slots-block-cap");
        let mut slots = stalled_slots(&dir, subagent_options());
        slots.on_hook(&start(A, 10));
        let slot = slots.registry.sessions[A].slot.unwrap();
        slots.registry.topic_created(slot, 100, "t", None);
        for i in 0..100 {
            let agent = format!("a{i:016}");
            slots
                .registry
                .confirm_subagent(&agent, A, format!("↳ Explore {agent}"));
        }
        slots.pump();
        slots.pump();
        let busy = slots
            .registry
            .subagents
            .values()
            .filter(|entry| entry.block.busy)
            .count();
        assert_eq!(busy, MAX_BLOCK_JOBS);
    }

    #[tokio::test]
    async fn a_late_body_read_never_overwrites_a_newer_one() {
        let dir = TempDir::new("slots-body-order");
        let mut slots = stalled_slots(&dir, subagent_options());
        slots.on_hook(&start(A, 10));
        slots
            .registry
            .confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        let gone = dir.path().join("agent-missing.jsonl");
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "First."));
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Second."));
        let mut done = slots.done_rx.take().unwrap();
        // Reads come back in the worst order: whatever is out, newest first.
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut batch = Vec::new();
            while let Ok(finished) = done.try_recv() {
                batch.push(finished);
            }
            if batch.is_empty() {
                break;
            }
            for finished in batch.into_iter().rev() {
                slots.on_done(finished);
            }
        }
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(format!("↳ Explore {S1}\nSecond.").as_str())
        );
    }

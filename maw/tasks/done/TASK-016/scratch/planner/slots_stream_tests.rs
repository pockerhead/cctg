
    // ---- Live transcript stream (TASK-016) ----

    const FAST: BucketConfig = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };

    fn stream_options() -> Options {
        Options {
            chat_id: CHAT,
            stream_every: Duration::from_millis(20),
            hold_answer: Duration::from_millis(400),
            ..options()
        }
    }

    /// A rig with a scheduler that never makes the stream wait.
    fn stream_rig(fake: Fake, options: Options, dir: TempDir) -> Rig {
        let fake = Arc::new(fake);
        let store = RegistryStore::open(dir.path()).unwrap();
        let registry = store.load().unwrap();
        let (scheduler, outbox) = Scheduler::new(fake.clone(), FAST);
        tokio::spawn(scheduler.run());
        let (slots, view) = Slots::new(registry, store, outbox, options);
        let (agents, agents_rx) = mpsc::channel(16);
        let (hooks, hooks_rx) = mpsc::channel(16);
        let (control, control_rx) = mpsc::unbounded_channel();
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
        Rig {
            fake,
            agents,
            hooks,
            control,
            view,
            dir,
            _to_agent: Vec::new(),
        }
    }

    /// `<dir>/projects/C--w/<session>.jsonl`, created empty.
    fn transcript_file(dir: &TempDir, session: &str) -> String {
        let project = dir.path().join("projects").join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        path.to_string_lossy().into_owned()
    }

    fn append(path: &str, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn typed(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    fn channel_record(message_id: i64) -> String {
        format!(
            "{{\"type\":\"user\",\"isMeta\":true,\"message\":{{\"role\":\"user\",\"content\":\"<channel source=\\\"cctg\\\" message_id=\\\"{message_id}\\\">hi</channel>\"}}}}\n"
        )
    }

    fn tool_call(id: &str, description: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"x\",\"description\":\"{description}\"}}}}]}}}}\n"
        )
    }

    fn tool_result(id: &str, error: Option<&str>) -> String {
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"{}\",\"is_error\":{}}}]}}}}\n",
            error.unwrap_or("ok"),
            error.is_some()
        )
    }

    fn start_with(session: &str, pid: u32, path: &str, source: &str) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            CWD.into(),
            path.into(),
            HookEvent::SessionStart {
                source: Some(source.into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    impl Rig {
        /// An agent that answers transcript reads from the real file, like
        /// `cctg agent` does, and keeps what else the hub sends it.
        async fn reader(&mut self, conn: u64, session: &str, pid: u32) -> mpsc::UnboundedReceiver<HubMsg> {
            let (to_agent, mut from_hub) = mpsc::channel(16);
            let (kept, kept_rx) = mpsc::unbounded_channel();
            let agents = self.agents.clone();
            tokio::spawn(async move {
                while let Some(msg) = from_hub.recv().await {
                    let HubMsg::TranscriptRead {
                        session_id,
                        path,
                        from,
                    } = msg
                    else {
                        let _ = kept.send(msg);
                        continue;
                    };
                    let chunk = crate::tail::read_chunk(&session_id, &path, from);
                    let event = AgentEvent::Message {
                        conn,
                        received_at: StdInstant::now(),
                        msg: chunk,
                    };
                    if agents.send(event).await.is_err() {
                        return;
                    }
                }
            });
            let register = Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(pid),
                verdict_ack: true,
                transcript_reads: true,
            };
            self.agents
                .send(AgentEvent::Registered {
                    conn,
                    register,
                    to_agent,
                })
                .await
                .unwrap();
            kept_rx
        }
    }

    /// Texts of new messages in `thread`, in order: sends and stream lines.
    fn topic_texts(ops: &[Op], thread: i64) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                }
                | Op::Stream {
                    thread_id: t, text, ..
                } if *t == thread => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn reactions(ops: &[Op]) -> Vec<(i64, String)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::React { message_id, emoji } => Some((*message_id, emoji.clone())),
                _ => None,
            })
            .collect()
    }

    async fn stream_texts(rig: &Rig, thread: i64, want: usize) -> Vec<String> {
        let ops = settled(rig, |ops| topic_texts(ops, thread).len() >= want).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = ops;
        topic_texts(&rig.fake.ops(), thread)
    }

    #[tokio::test]
    async fn appended_lines_reach_the_slot_topic_in_order_and_a_partial_line_waits() {
        let dir = TempDir::new("slots-stream-order");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;

        let second = tool_call("t2", "two");
        append(&path, &typed("go"));
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &second[..20]);
        let first = stream_texts(&rig, 100, 2).await;
        assert_eq!(first, ["> go", "• Bash: one ✓"]);

        // The rest of the cut line arrives: it is read whole, never lost.
        append(&path, &second[20..]);
        append(&path, &tool_result("t2", Some("boom")));
        let all = stream_texts(&rig, 100, 3).await;
        assert_eq!(all, ["> go", "• Bash: one ✓", "• Bash: two ✗ boom"]);
    }

    #[tokio::test]
    async fn a_restart_neither_repeats_nor_loses_stream_lines() {
        let dir = TempDir::new("slots-stream-restart");
        let path = transcript_file(&dir, A);
        let state = dir.path().to_path_buf();
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
        let saved = async {
            loop {
                let text = std::fs::read_to_string(state.join("registry.json")).unwrap_or_default();
                let offset = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["sessions"][A]["stream"]["offset"].as_u64());
                if offset == Some(std::fs::metadata(&path).unwrap().len()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, saved).await.expect("offset saved");

        // The hub goes away; the session writes on meanwhile.
        let Rig { dir, .. } = rig;
        append(&path, &tool_call("t2", "two"));
        append(&path, &tool_result("t2", None));
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        let _kept = rig.reader(2, A, 10).await;
        let after = stream_texts(&rig, 100, 1).await;
        assert_eq!(after, ["• Bash: two ✓"]);
    }

    #[tokio::test]
    async fn a_new_session_in_the_slot_streams_after_its_one_separator() {
        let dir = TempDir::new("slots-stream-rotation");
        let first = transcript_file(&dir, A);
        let second = transcript_file(&dir, B);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &first, "startup")).await;
        rig.ops_after(1).await;
        let _a = rig.reader(1, A, 10).await;
        append(&first, &typed("from A"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> from A"]);
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await;
        append(&second, &typed("from B"));
        rig.hook(start_with(B, 11, &second, "startup")).await;
        let _b = rig.reader(2, B, 11).await;
        let texts = stream_texts(&rig, 100, 3).await;
        assert_eq!(
            texts,
            ["> from A", "── session bbbbbbbb · new ──", "> from B"]
        );
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
    }

    #[derive(Default)]
    struct NoReactions(Fake);

    #[tokio::test]
    async fn eyes_on_hand_off_and_writing_only_for_the_same_messages_channel_record() {
        let dir = TempDir::new("slots-stream-reactions");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.control.send(say(Some(100), 42, Some("hi"))).unwrap();
        settled(&rig, |ops| reactions(ops) == [(42, "👀".to_owned())]).await;
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));

        // A prompt typed in the terminal, its UserPromptSubmit and the channel
        // record of a message this session never got change nothing.
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        append(&path, &typed("typed here"));
        append(&path, &channel_record(41));
        stream_texts(&rig, 100, 1).await;
        assert_eq!(reactions(&rig.fake.ops()), [(42, "👀".to_owned())]);

        append(&path, &channel_record(42));
        let ops = settled(&rig, |ops| reactions(ops).len() == 2).await;
        assert_eq!(
            reactions(&ops),
            [(42, "👀".to_owned()), (42, "✍".to_owned())]
        );
        // Its record again (a restart re-read) marks nothing twice.
        append(&path, &channel_record(42));
        append(&path, &typed("later"));
        stream_texts(&rig, 100, 2).await;
        assert_eq!(reactions(&rig.fake.ops()).len(), 2);
    }

    #[tokio::test]
    async fn a_refused_reaction_never_stops_routing() {
        let dir = TempDir::new("slots-stream-reaction-error");
        let fake = Fake {
            react_error: true,
            ..Fake::default()
        };
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(fake, stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        for id in [1, 2] {
            rig.control.send(say(Some(100), id, Some("m"))).unwrap();
        }
        for _ in 0..2 {
            let got = tokio::time::timeout(WAIT, kept.recv()).await.unwrap();
            assert!(matches!(got, Some(HubMsg::Inbound { .. })), "{got:?}");
        }
        append(&path, &typed("still streaming"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> still streaming"]);
    }

    #[tokio::test]
    async fn a_turn_answer_follows_the_lines_read_after_its_stop() {
        let dir = TempDir::new("slots-stream-hold");
        let path = transcript_file(&dir, A);
        let options = Options {
            // Only the read the Stop asks for can find the lines in time.
            stream_every: Duration::from_secs(3600),
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let mut rig = stream_rig(Fake::default(), options, dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        append(&path, &tool_call("t1", "last step"));
        append(&path, &tool_result("t1", None));
        rig.hook(stop(A, Some("done"))).await;
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: last step ✓", "done"]
        );
    }

    #[tokio::test]
    async fn a_held_answer_goes_out_when_the_agent_never_answers() {
        let dir = TempDir::new("slots-stream-hold-timeout");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        // Registers as a reader but never reads.
        let (to_agent, silent) = mpsc::channel(64);
        rig.agents
            .send(AgentEvent::Registered {
                conn: 1,
                register: Register {
                    session_id: A.into(),
                    host: "box".into(),
                    cwd: CWD.into(),
                    claude_pid: Some(10),
                    verdict_ack: true,
                    transcript_reads: true,
                },
                to_agent,
            })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("done anyway"))).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["done anyway"]);
        assert!(asked.elapsed() >= Duration::from_millis(300), "held first");
        drop(silent);
    }

    #[tokio::test]
    async fn an_agent_without_transcript_reads_is_never_asked() {
        let dir = TempDir::new("slots-stream-old-agent");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        rig.agent_with(1, A, Some(10), true).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        append(&path, &typed("not streamed"));
        rig.hook(stop(A, Some("answer at once"))).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["answer at once"]);
        assert!(received(&mut rig, 0).await.is_empty());
    }
}

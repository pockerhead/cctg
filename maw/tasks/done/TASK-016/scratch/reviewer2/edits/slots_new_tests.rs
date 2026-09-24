
    // ---- TASK-016 review 2: ordering barrier, delivery commit, reset ----

    /// The assistant text that ends a turn.
    fn answer_record(text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    }

    fn saved_offset(dir: &std::path::Path) -> Option<u64> {
        let text = std::fs::read_to_string(dir.join("registry.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["sessions"][A]["stream"]["offset"].as_u64())
    }

    async fn live_stream(options: Options, fake: Fake, name: &str) -> (Rig, String) {
        let dir = TempDir::new(name);
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(fake, options, dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        drop(rig.reader(1, A, 10).await);
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        (rig, path)
    }

    #[tokio::test]
    async fn a_turn_answer_waits_for_its_turn_end_even_when_the_file_lags() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-lag").await;
        rig.hook(stop(A, Some("done"))).await;
        // The file lags far behind the hook.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert!(topic_texts(&rig.fake.ops(), 100).is_empty(), "held");
        append(&path, &tool_call("t1", "late step"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("done"));
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: late step ✓", "done"]
        );
    }

    #[tokio::test]
    async fn every_stop_of_a_turn_follows_its_own_tool_lines() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-stops").await;
        // A blocking Stop hook of the user: the turn answers twice.
        rig.hook(stop(A, Some("first"))).await;
        rig.hook(stop(A, Some("second"))).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("first"));
        append(&path, &tool_call("t2", "two"));
        append(&path, &tool_result("t2", None));
        append(&path, &answer_record("second"));
        assert_eq!(
            stream_texts(&rig, 100, 4).await,
            ["• Bash: one ✓", "first", "• Bash: two ✓", "second"]
        );
    }

    #[tokio::test]
    async fn a_turn_end_read_before_its_stop_lets_the_answer_go_at_once() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-quick").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("quick"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("quick"))).await;
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: one ✓", "quick"]
        );
        assert!(asked.elapsed() < Duration::from_secs(10), "not held");
    }

    #[tokio::test]
    async fn a_refused_stream_message_is_sent_again_before_the_offset_moves() {
        let fake = Fake {
            stream_errors: Mutex::new(1),
            ..Fake::default()
        };
        let options = Options {
            stream_retry: Duration::from_millis(200),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, fake, "slots-stream-refused").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        // The refused attempt, then the one Telegram takes.
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: one ✓", "• Bash: one ✓"]
        );
        let len = std::fs::metadata(&path).unwrap().len();
        let state = rig.dir.path().to_path_buf();
        let saved = async {
            while saved_offset(&state) != Some(len) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, saved).await.expect("offset saved");
    }

    #[tokio::test]
    async fn a_refused_stream_message_comes_again_after_a_restart() {
        let fake = Fake {
            stream_errors: Mutex::new(usize::MAX),
            ..Fake::default()
        };
        let options = Options {
            stream_retry: Duration::from_secs(3600),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, fake, "slots-stream-refused-restart").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        settled(&rig, |ops| topic_texts(ops, 100).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let len = std::fs::metadata(&path).unwrap().len();
        assert_ne!(saved_offset(rig.dir.path()), Some(len), "not committed");

        let Rig { dir, .. } = rig;
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        let _kept = rig.reader(2, A, 10).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
    }

    #[tokio::test]
    async fn a_new_agent_process_goes_on_from_the_stream_position() {
        let (mut rig, path) =
            live_stream(stream_options(), Fake::default(), "slots-stream-agent-restart").await;
        append(&path, &typed("one"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> one"]);
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        append(&path, &typed("two"));
        let back = std::time::Instant::now();
        let _kept = rig.reader(2, A, 10).await;
        assert_eq!(stream_texts(&rig, 100, 2).await, ["> one", "> two"]);
        assert!(back.elapsed() < Duration::from_secs(5), "no read timeout");
    }

    #[tokio::test]
    async fn a_clear_in_the_same_process_streams_the_new_session_after_one_separator() {
        let dir = TempDir::new("slots-stream-clear");
        let first = transcript_file(&dir, A);
        let second = transcript_file(&dir, B);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &first, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        append(&first, &typed("from A"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> from A"]);
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        append(&second, &typed("from B"));
        rig.hook(start_with(B, 10, &second, "clear")).await;
        // The same agent (conn 1) now serves B.
        assert_eq!(
            stream_texts(&rig, 100, 3).await,
            ["> from A", "── session bbbbbbbb · new ──", "> from B"]
        );
        append(&first, &typed("late A"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(topic_texts(&rig.fake.ops(), 100).len(), 3);
    }

    #[tokio::test]
    async fn a_cut_transcript_is_read_again_from_its_start() {
        let (rig, path) =
            live_stream(stream_options(), Fake::default(), "slots-stream-cut").await;
        append(&path, &typed("before"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> before"]);
        std::fs::write(&path, typed("after")).unwrap();
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["> before", "> after"]
        );
    }

    #[tokio::test]
    async fn a_channel_record_of_another_server_leaves_the_reaction() {
        let (rig, path) =
            live_stream(stream_options(), Fake::default(), "slots-stream-foreign").await;
        rig.control.send(say(Some(100), 42, Some("hi"))).unwrap();
        settled(&rig, |ops| reactions(ops) == [(42, "👀".to_owned())]).await;
        append(
            &path,
            &channel_record(42).replace("source=\\\"cctg\\\"", "source=\\\"webhook\\\""),
        );
        append(&path, &typed("marker"));
        stream_texts(&rig, 100, 1).await;
        assert_eq!(reactions(&rig.fake.ops()), [(42, "👀".to_owned())]);
    }

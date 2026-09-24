
    fn line(thread: i64, text: &str) -> Op {
        Op::Stream {
            thread_id: thread,
            text: text.to_owned(),
            merge: true,
        }
    }

    fn sent_texts(fake: &Fake, thread: i64) -> Vec<String> {
        fake.calls()
            .iter()
            .filter_map(|call| match &call.op {
                Op::Stream {
                    thread_id, text, ..
                } if *thread_id == thread => Some(text.clone()),
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                } if *t == thread => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lines_go_one_per_message_while_the_budget_has_room() {
        let fake = Fake::new(&[]);
        let ops = vec![line(1, "a ✓"), line(1, "b ✓"), line(1, "c ✓")];
        let results = run(&fake, ops).await;
        assert!(results.iter().all(|r| matches!(r, Ok(Outcome::Sent(_)))));
        assert_eq!(sent_texts(&fake, 1), ["a ✓", "b ✓", "c ✓"]);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lines_held_back_by_the_limit_merge_in_order_without_loss() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..20).map(|i| line(1, &format!("t1-{i}"))).collect();
        ops.push(send(1, "answer"));
        ops.extend((20..30).map(|i| line(1, &format!("t1-{i}"))));
        ops.extend((0..5).map(|i| line(2, &format!("t2-{i}"))));
        let count = ops.len();
        let results = run(&fake, ops).await;
        assert_eq!(results.len(), count, "every line got an answer");
        assert!(results.iter().all(|r| matches!(
            r,
            Ok(Outcome::Sent(_) | Outcome::Merged)
        )));
        let topic: Vec<String> = sent_texts(&fake, 1);
        assert!(topic.len() < 21, "lines were merged: {topic:?}");
        let lines: Vec<&str> = topic.iter().flat_map(|text| text.split('\n')).collect();
        let mut want: Vec<String> = (0..20).map(|i| format!("t1-{i}")).collect();
        want.push("answer".to_owned());
        want.extend((20..30).map(|i| format!("t1-{i}")));
        assert_eq!(lines, want, "same lines, same order");
        // Nothing merges across the ordinary message of the topic.
        assert!(topic.contains(&"answer".to_owned()), "{topic:?}");
        let other: Vec<&str> = sent_texts(&fake, 2)
            .iter()
            .flat_map(|text| text.split('\n').map(str::to_owned).collect::<Vec<_>>())
            .map(|line| if line.starts_with("t2-") { "ok" } else { "wrong" })
            .collect();
        assert_eq!(other, ["ok"; 5]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_merged_message_stays_within_the_telegram_limit() {
        let fake = Fake::new(&[]);
        let long = "x".repeat(1500);
        let ops: Vec<Op> = (0..12).map(|_| line(1, &long)).collect();
        run(&fake, ops).await;
        let topic = sent_texts(&fake, 1);
        assert!(topic
            .iter()
            .all(|text| transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT));
        assert_eq!(topic.iter().map(|text| text.split('\n').count()).sum::<usize>(), 12);
    }

    #[tokio::test(start_paused = true)]
    async fn a_permission_prompt_overtakes_the_stream_lines_of_its_topic() {
        let fake = Fake::new(&[]);
        let mut ops: Vec<Op> = (0..8)
            .map(|i| Op::Stream {
                thread_id: 1,
                text: format!("s{i}"),
                merge: false,
            })
            .collect();
        ops.push(permission(1, "prompt"));
        run(&fake, ops).await;
        assert_eq!(texts(&fake)[0], "prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn reactions_are_unmetered_and_the_newest_one_per_message_wins() {
        let fake = Fake::new(&[]);
        let react = |id: i64, emoji: &str| Op::React {
            message_id: id,
            emoji: emoji.to_owned(),
        };
        let mut ops: Vec<Op> = (0..6).map(|i| send(1, &format!("m{i}"))).collect();
        ops.extend([react(5, "👀"), react(6, "👀"), react(5, "✍")]);
        let results = run(&fake, ops).await;
        assert!(matches!(results[6], Ok(Outcome::Superseded)));
        let reacts: Vec<(i64, String, Duration)> = fake
            .calls()
            .iter()
            .filter_map(|call| match &call.op {
                Op::React { message_id, emoji } => Some((*message_id, emoji.clone(), call.at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reacts,
            [
                (5, "✍".to_owned(), Duration::ZERO),
                (6, "👀".to_owned(), Duration::ZERO)
            ]
        );
    }
}

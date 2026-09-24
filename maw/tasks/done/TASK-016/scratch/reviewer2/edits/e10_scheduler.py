import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'ws')
P = os.path.join(WS, 'crates/cctg/src/hub/scheduler.rs')
edit(P, [
('''    /// This stream line went out inside the message of an earlier line of its
    /// topic, which got the actual answer.
    Merged,''', '''    /// This stream line went out inside the message of an earlier line of its
    /// topic, which got the actual answer. Only after that message was
    /// accepted: when it fails, the merged lines' receivers are dropped.
    Merged,'''),
('''            result => {
                let _ = job.reply.send(result);
                for merged in job.merged {
                    let _ = merged.send(Ok(Outcome::Merged));
                }
            }''', '''            result => {
                let accepted = result.is_ok();
                let _ = job.reply.send(result);
                // A refused message carried its merged lines with it: their
                // receivers close unanswered, never `Merged`.
                if accepted {
                    for merged in job.merged {
                        let _ = merged.send(Ok(Outcome::Merged));
                    }
                }
            }'''),
('''    /// Records calls with their (paused-clock) time; answers 429 for the
    /// first `flood` calls.
    struct Fake {
        start: Instant,
        calls: Mutex<Vec<Call>>,
        flood: Mutex<VecDeque<Duration>>,
        delay: Duration,
    }''', '''    /// Records calls with their (paused-clock) time; answers 429 for the
    /// first `flood` calls and a 502 for a message containing `refuse`.
    struct Fake {
        start: Instant,
        calls: Mutex<Vec<Call>>,
        flood: Mutex<VecDeque<Duration>>,
        delay: Duration,
        refuse: Option<&'static str>,
    }'''),
('''                flood: Mutex::new(flood.iter().copied().map(Duration::from_secs).collect()),
                delay,
            })
        }''', '''                flood: Mutex::new(flood.iter().copied().map(Duration::from_secs).collect()),
                delay,
                refuse: None,
            })
        }

        fn refusing(text: &'static str) -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                calls: Mutex::new(Vec::new()),
                flood: Mutex::new(VecDeque::new()),
                delay: Duration::ZERO,
                refuse: Some(text),
            })
        }'''),
('''            if let Some(wait) = flood {
                return Err(ApiError::RetryAfter(wait));
            }
            Ok(match op {''', '''            if let Some(wait) = flood {
                return Err(ApiError::RetryAfter(wait));
            }
            if let (Some(refuse), Op::Stream { text, .. }) = (self.refuse, op)
                && text.contains(refuse)
            {
                return Err(ApiError::Telegram {
                    code: 502,
                    description: "Bad Gateway".to_owned(),
                });
            }
            Ok(match op {'''),
('''    #[tokio::test(start_paused = true)]
    async fn a_merged_message_stays_within_the_telegram_limit() {''', '''    #[tokio::test(start_paused = true)]
    async fn a_refused_merged_message_answers_none_of_its_lines_as_merged() {
        let fake = Fake::refusing("t-7\\n");
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let mut receivers = Vec::new();
        for i in 0..20 {
            receivers.push(outbox.submit(line(1, &format!("t-{i}"))).await);
        }
        drop(outbox);
        scheduler.run().await;
        let mut answers = Vec::new();
        for receiver in receivers {
            answers.push(receiver.await.ok());
        }
        let refused = fake
            .calls()
            .into_iter()
            .find_map(|call| match call.op {
                Op::Stream { text, .. } if text.contains("t-7\\n") => Some(text),
                _ => None,
            })
            .expect("t-7 went out merged with the next line");
        for (i, answer) in answers.iter().enumerate() {
            let in_refused = refused.split('\\n').any(|line| line == format!("t-{i}"));
            match answer {
                Some(Ok(Outcome::Sent(_) | Outcome::Merged)) => assert!(!in_refused, "t-{i}"),
                Some(Err(_)) | None => assert!(in_refused, "t-{i}: {answer:?}"),
                other => panic!("t-{i}: {other:?}"),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_merged_message_stays_within_the_telegram_limit() {'''),
])
print('ok')

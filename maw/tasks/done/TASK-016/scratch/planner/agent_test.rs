
    #[tokio::test]
    async fn a_transcript_read_is_answered_over_the_link_and_never_reaches_claude() {
        let dir = crate::hub::testdir::TempDir::new("agent-transcript-read");
        let session = "5e551017-0000-4000-8000-000000000001";
        let project = dir.path().join("projects").join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        )
        .unwrap();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let mut claude = claude(Hub::Link(outbox), Some(events));
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (mut reader, mut write) = raw_hub(&listener).await;
        wire::write_msg(
            &mut write,
            &HubMsg::TranscriptRead {
                session_id: session.into(),
                path: path.to_string_lossy().into_owned(),
                from: Some(0),
            },
        )
        .await
        .unwrap();
        match agent_line(&mut reader).await {
            AgentMsg::TranscriptChunk {
                from: 0,
                to,
                lines,
                missing: false,
                ..
            } => {
                assert_eq!(to, std::fs::metadata(&path).unwrap().len());
                assert_eq!(
                    lines[0].items,
                    [wire::StreamItem::Prompt {
                        text: "hello".into()
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
        // Claude Code saw nothing of it: the next line it gets is the answer
        // to its own request.
        claude
            .send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 5);
    }
}

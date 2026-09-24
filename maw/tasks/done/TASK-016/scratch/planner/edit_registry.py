import os
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws/crates/cctg/src/hub/registry.rs')
s = open(p, encoding='utf-8').read()


def rep(a, b):
    global s
    assert s.count(a) == 1, a[:80]
    s = s.replace(a, b)


rep("""    /// The `⇣ nested` block of a nested run in its parent's topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Block>,
}
""", """    /// The `⇣ nested` block of a nested run in its parent's topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Block>,
    /// The live transcript stream of a top-level session (TASK-016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Stream>,
}

/// What survives a restart of a session's transcript stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    /// Transcript bytes whose stream messages Telegram has answered (the
    /// next read after a restart starts here). `None`: nothing read yet;
    /// the first read starts at the end of the file, so a session resumed
    /// from before the hub knew it does not replay its history.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Tool calls read before `offset` whose line waits for its result.
    #[serde(default)]
    pub calls: Vec<PendingCall>,
    /// Telegram messages handed to the session's agent that show 👀 and wait
    /// for their channel record to turn ✍.
    #[serde(default)]
    pub receipts: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    pub id: String,
    /// The `/brief` line of the call.
    pub line: String,
    /// End of the line with its result, once read; the call is forgotten
    /// when `offset` passes it.
    #[serde(default)]
    pub result_end: Option<u64>,
}
""")
rep("""                agent: None,
                waiting: false,
                block: None,
            });
        entry.kind = kind.clone();""", """                agent: None,
                waiting: false,
                block: None,
                stream: None,
            });
        entry.kind = kind.clone();""")
rep("""        if claude_pid.is_some() {
            entry.claude_pid = claude_pid;
        }
        if let SessionKind::Nested { parent: Some(_) } = &kind""", """        if claude_pid.is_some() {
            entry.claude_pid = claude_pid;
        }
        if kind == SessionKind::TopLevel && entry.stream.is_none() {
            // A new transcript is streamed from its first byte; any other
            // start of a session the stream never saw begins at the end.
            let fresh = matches!(source, Some("startup" | "clear"));
            entry.stream = Some(Stream {
                offset: fresh.then_some(0),
                ..Stream::default()
            });
        }
        if let SessionKind::Nested { parent: Some(_) } = &kind""")
# test
rep("""    #[test]
    fn a_resumed_session_returns_to_its_slot() {""", """    #[test]
    fn a_new_transcript_streams_from_its_start_and_a_resume_keeps_its_offset() {
        let mut registry = Registry::default();
        registry.apply_hook(&start("s1", "/w", Some(1), None));
        let offset = |registry: &Registry, id: &str| {
            registry.sessions[id].stream.as_ref().map(|stream| stream.offset)
        };
        assert_eq!(offset(&registry, "s1"), Some(Some(0)));
        registry.sessions.get_mut("s1").unwrap().stream.as_mut().unwrap().offset = Some(420);
        registry.apply_hook(&end("s1"));
        registry.apply_hook(&start_from("s1", "/w", Some(2), None, "resume"));
        assert_eq!(offset(&registry, "s1"), Some(Some(420)));
        // A resume the hub never saw starts at the end of the file.
        registry.apply_hook(&start_from("s2", "/w", Some(3), None, "resume"));
        assert_eq!(offset(&registry, "s2"), Some(None));
        // Nested runs are not streamed.
        registry.apply_hook(&start("s3", "/w", Some(4), Some(3)));
        assert_eq!(offset(&registry, "s3"), None);
        let bytes = RegistryStore::encode(&registry);
        let back: Registry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(offset(&back, "s1"), Some(Some(420)));
    }

    #[test]
    fn a_resumed_session_returns_to_its_slot() {""")
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')

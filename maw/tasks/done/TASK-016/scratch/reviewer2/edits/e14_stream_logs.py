import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'ws')
P = os.path.join(WS, 'crates/cctg/tests/stream_logs.rs')
edit(P, [
('''//! Log capture for the live transcript stream: a transcript that is missing
//! (not written yet, or deleted) is warned about once while the stream keeps
//! asking, and it goes on once the file is there. Paths and message text never
//! reach the logs.''', '''//! Log capture for the live transcript stream: a transcript that is missing
//! (not written yet, or deleted) is warned about once while the stream keeps
//! asking, and it goes on once the file is there; a transcript cut below the
//! stream's offset is warned about once and read again from its start. Paths
//! and message text never reach the logs.'''),
('''    let (to_agent, mut from_hub) = mpsc::channel(16);
    let reads = agents.clone();''', '''    let (to_agent, mut from_hub) = mpsc::channel(16);
    let reads = agents.clone();
    let root = state.join("projects");'''),
('''                let msg = cctg::tail::read_chunk(&session_id, &path, from);''',
 '''                let msg = cctg::tail::read_chunk(Some(&root), &session_id, &path, from);'''),
('''    assert_eq!(streamed(&fake), [format!("> {secret_text}")]);
    let _ = std::fs::remove_dir_all(&state);''', '''    assert_eq!(streamed(&fake), [format!("> {secret_text}")]);

    // Cut below the offset: one warning, then the file is read from its start.
    std::fs::write(&transcript, "").expect("cut");
    let polls = *asked.lock().expect("count") + 10;
    while *asked.lock().expect("count") < polls {
        assert!(Instant::now() < deadline, "the stream stopped asking");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let after_cut = format!("after the cut {pid}");
    std::fs::write(
        &transcript,
        format!("{{\\"type\\":\\"user\\",\\"message\\":{{\\"role\\":\\"user\\",\\"content\\":\\"{after_cut}\\"}}}}\\n"),
    )
    .expect("transcript");
    while streamed(&fake).len() < 2 {
        assert!(Instant::now() < deadline, "the stream never went on after the cut");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        streamed(&fake),
        [format!("> {secret_text}"), format!("> {after_cut}")]
    );
    let _ = std::fs::remove_dir_all(&state);'''),
('''    assert_eq!(
        logs.matches("session transcript not found").count(),
        1,
        "{logs}"
    );
    for private in [
        secret_text.as_str(),''', '''    assert_eq!(
        logs.matches("session transcript not found").count(),
        1,
        "{logs}"
    );
    assert_eq!(logs.matches("was cut or replaced").count(), 1, "{logs}");
    for private in [
        secret_text.as_str(),
        after_cut.as_str(),'''),
])
print('ok')

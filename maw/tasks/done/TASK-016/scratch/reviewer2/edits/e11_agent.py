import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'ws')
P = os.path.join(WS, 'crates/cctg/src/agent.rs')
edit(P, [
('''    if let Err(error) = serve_channel(frames, tokio::io::stdout(), hub, events).await {''',
 '''    let projects = tail::projects_root();
    if let Err(error) = serve_channel(frames, tokio::io::stdout(), hub, events, projects).await {'''),
('''/// The MCP loop: stdin frames and hub events in, JSON-RPC lines out. Returns
/// when stdin ends (the hub link stops with it) or stdout fails.
pub async fn serve_channel<W: AsyncWrite + Unpin>(
    mut frames: mpsc::Receiver<Frame>,
    mut output: W,
    hub: Hub,
    mut events: Option<mpsc::Receiver<LinkEvent>>,
) -> std::io::Result<()> {
    let reads = match &hub {
        Hub::Link(outbox) => Some(outbox.clone()),
        Hub::Off(_) => None,
    };''', '''/// The MCP loop: stdin frames and hub events in, JSON-RPC lines out. Returns
/// when stdin ends (the hub link stops with it) or stdout fails. Transcript
/// reads are answered only from under `projects` ([`tail::projects_root`]).
pub async fn serve_channel<W: AsyncWrite + Unpin>(
    mut frames: mpsc::Receiver<Frame>,
    mut output: W,
    hub: Hub,
    mut events: Option<mpsc::Receiver<LinkEvent>>,
    projects: Option<PathBuf>,
) -> std::io::Result<()> {
    let reads = match &hub {
        Hub::Link(outbox) => Some(spawn_reader(outbox.clone(), projects)),
        Hub::Off(_) => None,
    };'''),
('''                Some(LinkEvent::Message(HubMsg::TranscriptRead { session_id, path, from })) => {
                    if let Some(outbox) = &reads {
                        spawn_read(outbox.clone(), session_id, path, from);
                    }
                    Vec::new()
                }''', '''                Some(LinkEvent::Message(HubMsg::TranscriptRead { session_id, path, from })) => {
                    // One read at a time: while one runs, a request waits in
                    // the slot and a further one is dropped (the hub asks
                    // again after its timeout).
                    if let Some(reads) = &reads
                        && reads.try_send((session_id, path, from)).is_err()
                    {
                        debug!("transcript read busy; request dropped");
                    }
                    Vec::new()
                }'''),
('''/// Reads the asked transcript chunk off the loop and queues the answer for
/// the hub; the hub asks again if it gets lost with the link.
fn spawn_read(outbox: mpsc::Sender<AgentMsg>, session_id: String, path: String, from: Option<u64>) {
    tokio::spawn(async move {
        let chunk =
            tokio::task::spawn_blocking(move || tail::read_chunk(&session_id, &path, from)).await;
        if let Ok(chunk) = chunk
            && outbox.send(chunk).await.is_err()
        {
            debug!("hub link gone; transcript chunk dropped");
        }
    });
}''', '''type ReadRequest = (String, String, Option<u64>);

/// The one worker that reads transcript chunks off the loop, one blocking
/// read at a time, and queues each answer for the hub; the hub asks again if
/// an answer gets lost with the link.
fn spawn_reader(outbox: mpsc::Sender<AgentMsg>, projects: Option<PathBuf>) -> mpsc::Sender<ReadRequest> {
    let (requests, mut pending) = mpsc::channel::<ReadRequest>(1);
    tokio::spawn(async move {
        while let Some((session_id, path, from)) = pending.recv().await {
            let root = projects.clone();
            let chunk = tokio::task::spawn_blocking(move || {
                tail::read_chunk(root.as_deref(), &session_id, &path, from)
            })
            .await;
            if let Ok(chunk) = chunk
                && outbox.send(chunk).await.is_err()
            {
                debug!("hub link gone; transcript chunk dropped");
                return;
            }
        }
    });
    requests
}'''),
('''    fn claude(hub: Hub, events: Option<mpsc::Receiver<LinkEvent>>) -> Claude {
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        tokio::spawn(serve_channel(frames_rx, ours, hub, events));''', '''    fn claude(hub: Hub, events: Option<mpsc::Receiver<LinkEvent>>) -> Claude {
        claude_reading(hub, events, None)
    }

    fn claude_reading(
        hub: Hub,
        events: Option<mpsc::Receiver<LinkEvent>>,
        projects: Option<PathBuf>,
    ) -> Claude {
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        tokio::spawn(serve_channel(frames_rx, ours, hub, events, projects));'''),
('''        let serving = tokio::spawn(serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
        ));''', '''        let serving = tokio::spawn(serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
            None,
        ));'''),
('''        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let mut claude = claude(Hub::Link(outbox), Some(events));
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (mut reader, mut write) = raw_hub(&listener).await;''', '''        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let mut claude = claude_reading(
            Hub::Link(outbox),
            Some(events),
            Some(dir.path().join("projects")),
        );
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (mut reader, mut write) = raw_hub(&listener).await;'''),
])
print('ok')

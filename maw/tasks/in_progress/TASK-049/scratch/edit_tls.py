import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/Cargo.toml'); rep = f.rep
rep("""serde.workspace = true
serde_json.workspace = true
""","""serde.workspace = true
serde_json.workspace = true
# TCP keepalive on the agent link (TASK-049): neither std nor tokio sets its
# idle time. Already in the lock file through tokio; no new download.
socket2 = "0.6"
""")
f.save()
f = Sub('crates/cctg/src/tls.rs'); rep = f.rep
rep("""use std::task::{Context, Poll};
""","""use std::task::{Context, Poll};
use std::time::Duration;
""")
rep("""use rustls::{DigitallySignedStruct, SignatureScheme};
""","""use rustls::{DigitallySignedStruct, SignatureScheme};
use socket2::{SockRef, TcpKeepalive};
""")
rep("""/// Whether plain TCP (the secret in the clear) may go to `peer`: a
""","""/// Idle time before the first TCP keepalive probe, and between probes
/// (TASK-049): traffic every half minute keeps a NAT or tunnel on the way
/// from forgetting a quiet connection. The link's own heartbeat
/// ([`crate::wire::Heartbeat`]) finds one that is gone anyway.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Turns on TCP keepalive; a socket that refuses it works as before.
fn keepalive(tcp: &TcpStream) {
    let params = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    if let Err(error) = SockRef::from(tcp).set_tcp_keepalive(&params) {
        tracing::debug!(kind = ?error.kind(), "TCP keepalive not set");
    }
}

/// Whether plain TCP (the secret in the clear) may go to `peer`: a
""")
rep("""        let tcp = TcpStream::connect(self.addr.as_str()).await?;
        let _ = tcp.set_nodelay(true);
""","""        let tcp = TcpStream::connect(self.addr.as_str()).await?;
        let _ = tcp.set_nodelay(true);
        keepalive(&tcp);
""")
rep("""    pub async fn accept(&self, tcp: TcpStream) -> io::Result<Stream> {
        let _ = tcp.set_nodelay(true);
""","""    pub async fn accept(&self, tcp: TcpStream) -> io::Result<Stream> {
        let _ = tcp.set_nodelay(true);
        keepalive(&tcp);
""")
rep("""    #[test]
    fn broken_files_are_named_but_not_quoted() {""","""    fn tcp_of(stream: &Stream) -> &TcpStream {
        match stream {
            Stream::Plain(tcp) => tcp,
            Stream::Tls(tls) => tls.get_ref().0,
        }
    }

    /// TASK-049: both ends of both kinds of link have TCP keepalive on.
    #[tokio::test]
    async fn both_ends_keep_the_connection_alive() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let accepting = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            Incoming::plain().accept(tcp).await.unwrap()
        });
        let client = HubAddr::plain(&addr).connect().await.unwrap();
        let server = accepting.await.unwrap();
        for stream in [&client, &server] {
            assert!(SockRef::from(tcp_of(stream)).keepalive().unwrap());
        }

        let (chain, key) = certified("localhost");
        let (acceptor, pin) = Acceptor::new(chain, key).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let accepting = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            Incoming::tls(acceptor).accept(tcp).await.unwrap()
        });
        let client = HubAddr::pinned(&addr, pin).unwrap().connect().await.unwrap();
        let server = accepting.await.unwrap();
        for stream in [&client, &server] {
            assert!(matches!(stream, Stream::Tls(_)));
            assert!(SockRef::from(tcp_of(stream)).keepalive().unwrap());
        }
    }

    #[test]
    fn broken_files_are_named_but_not_quoted() {""")
f.save()

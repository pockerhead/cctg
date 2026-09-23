//! Hook end of the hub ingress: one HTTP POST per call, one overall timeout,
//! no retry loop. Plain TCP instead of `reqwest`: the body is tiny, the hub is
//! local or on a private network, and a TLS-capable client costs start-up
//! time the `SessionEnd` budget (1.5 s shared) cannot spare.
//!
//! [`HookPost::new`](crate::wire::HookPost::new) mints the event id; calling
//! [`post`] again with the same value re-sends the same event, which the hub
//! drops as a repeat.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::wire::{HOOK_PATH, HookPost, Secret};

const MAX_STATUS_LINE: u64 = 256;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error("hub did not answer within {0:?}")]
    Timeout(Duration),
    #[error("cannot reach the hub: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("hub answered HTTP {0}")]
    Status(u16),
    #[error("hub answer is not HTTP")]
    BadResponse,
}

/// Sends `post` to the hub hook endpoint at `addr` (`host:port`). Everything,
/// connect included, fits in `timeout`. `Ok` means the hub has the event.
pub async fn post(
    addr: &str,
    secret: &Secret,
    post: &HookPost,
    timeout: Duration,
) -> Result<(), PostError> {
    let body = serde_json::to_vec(post).expect("hook posts always serialize");
    let head = format!(
        "POST {HOOK_PATH} HTTP/1.1\r\nHost: cctg-hub\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        secret.expose(),
        body.len()
    );
    let exchange = async {
        let io = |error: std::io::Error| PostError::Io(error.kind());
        let mut stream = TcpStream::connect(addr).await.map_err(io)?;
        let _ = stream.set_nodelay(true);
        stream
            .write_all(&[head.as_bytes(), &body].concat())
            .await
            .map_err(io)?;
        let mut status_line = Vec::new();
        BufReader::new(stream)
            .take(MAX_STATUS_LINE)
            .read_until(b'\n', &mut status_line)
            .await
            .map_err(io)?;
        match parse_status(&status_line) {
            Some(204) => Ok(()),
            Some(code) => Err(PostError::Status(code)),
            None => Err(PostError::BadResponse),
        }
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .unwrap_or(Err(PostError::Timeout(timeout)))
}

/// Accepts only a complete `HTTP/1.1 NNN[ reason]\r\n` line: a truncated or
/// foreign answer must never count as delivered.
fn parse_status(line: &[u8]) -> Option<u16> {
    let rest = line.strip_suffix(b"\r\n")?.strip_prefix(b"HTTP/1.1 ")?;
    let (code, tail) = rest.split_at_checked(3)?;
    if !code.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {
        return None;
    }
    std::str::from_utf8(code).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::hub::ingress;
    use crate::wire::HookEvent;

    const SECRET: &str = "0123456789abcdef-secret";

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    fn sample() -> HookPost {
        HookPost::new(
            "box".into(),
            "5e551017-0000-4000-8000-000000000001".into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionEnd {
                reason: Some("other".into()),
            },
        )
    }

    async fn hub() -> (String, mpsc::Receiver<HookPost>) {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(ingress::serve_hooks(listener, secret(), tx));
        (addr, rx)
    }

    #[tokio::test]
    async fn a_resent_post_reaches_the_hub_once() {
        let (addr, mut events) = hub().await;
        let sent = sample();
        let timeout = Duration::from_secs(5);
        let started = Instant::now();
        post(&addr, &secret(), &sent, timeout).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        post(&addr, &secret(), &sent, timeout).await.unwrap();
        assert_eq!(events.recv().await, Some(sent));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_wrong_secret_is_refused() {
        let (addr, mut events) = hub().await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        let result = post(&addr, &wrong, &sample(), Duration::from_secs(5)).await;
        assert_eq!(result, Err(PostError::Status(401)));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_silent_hub_costs_at_most_the_timeout() {
        // Accepts and never answers.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let _held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream);
            }
        });
        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let result = post(&addr, &secret(), &sample(), timeout).await;
        assert_eq!(result, Err(PostError::Timeout(timeout)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn no_hub_is_an_error_within_the_timeout() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let timeout = Duration::from_millis(800);
        let started = Instant::now();
        let error = post(&addr, &secret(), &sample(), timeout)
            .await
            .unwrap_err();
        assert!(
            matches!(error, PostError::Io(_) | PostError::Timeout(_)),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(!format!("{error} {error:?}").contains(SECRET));
    }

    #[test]
    fn status_lines() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_status(b"HTTP/1.1 401 Unauthorized\r\n"), Some(401));
        assert_eq!(parse_status(b"HTTP/1.1 204\r\n"), Some(204));
        for bad in [
            &b"HTTP/1.1 204 No Content"[..],
            b"HTTP/1.1 204",
            b"HTTP/1.1 204 No Content\n",
            b"HTTP/1.x 204 No Content\r\n",
            b"HTTP/1.0 204 No Content\r\n",
            b"HTTP/1.1 20 No Content\r\n",
            b"HTTP/1.1 2044 No Content\r\n",
            b"HTTP/1.1 2x4 No Content\r\n",
            b"HTTP/1.1  204 No Content\r\n",
            b"SSH-2.0-x\r\n",
            b"",
        ] {
            assert_eq!(parse_status(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    /// A fake hub that reads the request, writes `answer` and closes.
    async fn answering(answer: &'static [u8]) -> String {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = vec![0u8; 64 * 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(answer).await;
                let _ = stream.shutdown().await;
                let _ = stream.read_to_end(&mut request).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_truncated_or_foreign_status_line_is_not_success() {
        let timeout = Duration::from_secs(5);
        for answer in [
            &b"HTTP/1.1 204"[..],
            b"HTTP/1.x 204 No Content\r\n\r\n",
            b"HTTP/1.1 2040 No Content\r\n\r\n",
            b"",
        ] {
            let addr = answering(answer).await;
            let result = post(&addr, &secret(), &sample(), timeout).await;
            assert_eq!(
                result,
                Err(PostError::BadResponse),
                "{}",
                String::from_utf8_lossy(answer)
            );
        }
        let addr = answering(b"HTTP/1.1 204 No Content\r\n\r\n").await;
        assert_eq!(post(&addr, &secret(), &sample(), timeout).await, Ok(()));
    }
}

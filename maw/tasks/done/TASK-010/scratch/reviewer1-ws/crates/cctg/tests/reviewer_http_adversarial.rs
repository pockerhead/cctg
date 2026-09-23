use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use cctg::hub::ingress;
use cctg::wire::{HookPost, Secret};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[tokio::test]
async fn malformed_http_version_is_not_accepted() {
    let secret_text = "0123456789abcdef-secret";
    let secret = Secret::parse(secret_text).unwrap();
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::channel::<HookPost>(4);
    let server = tokio::spawn(ingress::serve_hooks(listener, secret, tx));

    let body = br#"{"v":1,"event_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","host":"h","session_id":"s","event":{"type":"session_end"}}"#;
    let request = format!(
        "POST /v1/hook HTTP/1.x\r\nAuthorization: Bearer {secret_text}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();

    assert!(
        !response.starts_with(b"HTTP/1.1 204"),
        "malformed HTTP version was accepted: {}",
        String::from_utf8_lossy(&response)
    );
    assert!(
        rx.try_recv().is_err(),
        "malformed request delivered an event"
    );
    server.abort();
}

#[tokio::test]
async fn early_unauthorized_response_survives_a_maximum_sized_body() {
    let secret_text = "0123456789abcdef-secret";
    let secret = Secret::parse(secret_text).unwrap();
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, _rx) = mpsc::channel::<HookPost>(4);
    let server = tokio::spawn(ingress::serve_hooks(listener, secret, tx));

    let body = vec![b'x'; cctg::wire::MAX_HOOK_BODY];
    let head = format!(
        "POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer 0123456789abcdef-wrong\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let write_result = stream.write_all(&[head.as_bytes(), &body].concat()).await;
    let mut response = Vec::new();
    let read_result =
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut response)).await;

    assert!(
        write_result.is_ok()
            && matches!(read_result, Ok(Ok(_)))
            && response.starts_with(b"HTTP/1.1 401"),
        "early response was lost: write={write_result:?}, read={read_result:?}, response={}",
        String::from_utf8_lossy(&response)
    );
    server.abort();
}

#[tokio::test]
async fn malformed_header_name_is_not_accepted() {
    let secret_text = "0123456789abcdef-secret";
    let secret = Secret::parse(secret_text).unwrap();
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::channel::<HookPost>(4);
    let server = tokio::spawn(ingress::serve_hooks(listener, secret, tx));
    let body = br#"{"v":1,"event_id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","host":"h","session_id":"s","event":{"type":"session_end"}}"#;
    let request = format!(
        "POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer {secret_text}\r\nBad(Name: x\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 400"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert!(rx.try_recv().is_err());
    server.abort();
}

#[tokio::test]
async fn a_pipelined_second_request_is_never_processed() {
    let secret_text = "0123456789abcdef-secret";
    let secret = Secret::parse(secret_text).unwrap();
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel::<HookPost>(4);
    let server = tokio::spawn(ingress::serve_hooks(listener, secret, tx));
    let body = br#"{"v":1,"event_id":"cccccccccccccccccccccccccccccccc","host":"h","session_id":"s","event":{"type":"session_end"}}"#;
    let head = format!(
        "POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer {secret_text}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut raw = [head.as_bytes(), body].concat();
    raw.extend_from_slice(&raw.clone());
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(&raw).await.unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rx.len() <= 1, "a one-request connection delivered twice");
    server.abort();
}

#[tokio::test]
async fn an_incomplete_slow_header_is_closed_on_the_deadline() {
    let secret = Secret::parse("0123456789abcdef-secret").unwrap();
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::channel::<HookPost>(4);
    let server = tokio::spawn(ingress::serve_hooks(listener, secret, tx));
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"POST /v1/hook HTTP/1.1\r\nX:")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
        .await
        .expect("server must close after its two-second deadline")
        .ok();
    assert!(rx.try_recv().is_err());
    server.abort();
}

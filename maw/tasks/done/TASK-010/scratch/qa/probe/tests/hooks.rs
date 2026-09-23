//! QA probes for the hook HTTP ingress and the hook client.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use cctg::hook::{self, PostError};
use cctg::hub::ingress;
use cctg::wire::{HOOK_PATH, HookEvent, HookPost, Secret};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SECRET: &str = "qa-secret-0123456789abcdef";

fn secret() -> Secret {
    Secret::parse(SECRET).unwrap()
}

async fn hub(queue: usize) -> (SocketAddr, mpsc::Receiver<HookPost>) {
    let l = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await.unwrap();
    let a = l.local_addr().unwrap();
    let (tx, rx) = mpsc::channel(queue);
    tokio::spawn(ingress::serve_hooks(l, secret(), tx));
    (a, rx)
}

fn sample() -> HookPost {
    HookPost::new(
        "qa".into(),
        "11111111-2222-4333-8444-555555555555".into(),
        "/qa".into(),
        "/qa/s.jsonl".into(),
        HookEvent::SessionStart { source: Some("startup".into()), claude_pid: Some(1), parent_claude_pid: None },
    )
}

async fn status(addr: SocketAddr, raw: &[u8]) -> Option<u16> {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _ = s.write_all(raw).await;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    std::str::from_utf8(out.get(9..12)?).ok()?.parse().ok()
}

fn req(headers: &str, body: &[u8]) -> Vec<u8> {
    [format!("POST {HOOK_PATH} HTTP/1.1\r\n{headers}\r\n").as_bytes(), body].concat()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_http_edge_cases() {
    let (addr, mut rx) = hub(64).await;
    let b = serde_json::to_vec(&sample()).unwrap();
    let n = b.len();
    let cases: Vec<(&str, Vec<u8>, u16)> = vec![
        ("two identical good Authorization", req(&format!("Authorization: Bearer {SECRET}\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {n}\r\n"), &b), 400),
        ("good then bad Authorization", req(&format!("Authorization: Bearer {SECRET}\r\nAuthorization: Bearer nope-nope-nope-nope\r\nContent-Length: {n}\r\n"), &b), 400),
        ("lowercase header + scheme", req(&format!("authorization: bearer {SECRET}\r\ncontent-length: {n}\r\n"), &b), 204),
        ("Bearer with TAB separator", req(&format!("Authorization: Bearer\t{SECRET}\r\nContent-Length: {n}\r\n"), &b), 401),
        ("Basic scheme", req(&format!("Authorization: Basic {SECRET}\r\nContent-Length: {n}\r\n"), &b), 401),
        ("secret with extra suffix", req(&format!("Authorization: Bearer {SECRET}x\r\nContent-Length: {n}\r\n"), &b), 401),
        ("secret prefix", req(&format!("Authorization: Bearer {}\r\nContent-Length: {n}\r\n", &SECRET[..SECRET.len() - 1]), &b), 401),
        ("CL shorter than body (pipelined junk)", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: {}\r\n", n - 1), &b), 400),
        ("CL with leading zeros", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: 000{n}\r\n"), &b), 204),
        ("CL with tab OWS", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length:\t{n}\t\r\n"), &b), 204),
        ("CL empty", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: \r\n"), &b), 400),
        ("query in target", [format!("POST {HOOK_PATH}?x=1 HTTP/1.1\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {n}\r\n\r\n").as_bytes(), &b[..]].concat(), 404),
        ("absolute-form target", [format!("POST http://h{HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {n}\r\n\r\n").as_bytes(), &b[..]].concat(), 404),
        ("lowercase method", [format!("post {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {n}\r\n\r\n").as_bytes(), &b[..]].concat(), 405),
        ("bare LF line endings", [format!("POST {HOOK_PATH} HTTP/1.1\nAuthorization: Bearer {SECRET}\nContent-Length: {n}\n\n").as_bytes(), &b[..]].concat(), 0),
        ("non-UTF8 header value", [format!("POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {n}\r\nX: ").as_bytes(), &[0xff, 0xfe], b"\r\n\r\n", &b[..]].concat(), 400),
        ("header line without colon", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: {n}\r\nNoColon\r\n"), &b), 400),
        ("empty header name", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: {n}\r\n: x\r\n"), &b), 400),
        ("TE lowercase identity", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: {n}\r\ntransfer-encoding: identity\r\n"), &b), 400),
        ("missing auth + huge CL", req("Content-Length: 99999999\r\n", b""), 401),
        ("NBSP padded CL (lenient trim?)", req(&format!("Authorization: Bearer {SECRET}\r\nContent-Length: \u{a0}{n}\r\n"), &b), 400),
    ];
    let mut failures = Vec::new();
    for (name, raw, want) in cases {
        let got = status(addr, &raw).await;
        let ok = if want == 0 { got.is_none() || got == Some(400) } else { got == Some(want) };
        eprintln!("QA http {name}: got {got:?} want {want}");
        if !ok {
            failures.push(format!("{name}: got {got:?}, want {want}"));
        }
    }
    let mut delivered = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        delivered += 1;
    }
    eprintln!("QA http delivered events: {delivered}");
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_header_limit_exact_even_byte_by_byte() {
    let (addr, mut rx) = hub(8).await;
    let b = serde_json::to_vec(&sample()).unwrap();
    // head = bytes before the final CRLFCRLF
    let prefix = format!("POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {SECRET}\r\nContent-Length: {}\r\nX-Pad: ", b.len());
    let make = |head_len: usize| [prefix.as_bytes(), &vec![b'p'; head_len - prefix.len()], b"\r\n\r\n", &b[..]].concat();
    assert_eq!(status(addr, &make(8192)).await, Some(204));
    assert!(rx.recv().await.is_some());
    assert_eq!(status(addr, &make(8193)).await, Some(431));
    // at the limit, delivered one byte at a time
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.set_nodelay(true).unwrap();
    let mut fresh = sample();
    fresh.event_id = cctg::wire::EventId::new();
    let b2 = serde_json::to_vec(&fresh).unwrap();
    assert_eq!(b2.len(), b.len());
    let raw = [prefix.as_bytes(), &vec![b'p'; 8192 - prefix.len()], b"\r\n\r\n", &b2[..]].concat();
    let t = Instant::now();
    for chunk in raw.chunks(97) {
        s.write_all(chunk).await.unwrap();
    }
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    assert!(out.starts_with(b"HTTP/1.1 204"), "{}", String::from_utf8_lossy(&out));
    eprintln!("QA chunked-at-limit took {:?}", t.elapsed());
    assert!(rx.recv().await.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_concurrent_resends_of_one_event_deliver_once() {
    let (addr, mut rx) = hub(64).await;
    let p = sample();
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let p = p.clone();
        let a = addr.to_string();
        tasks.push(tokio::spawn(async move { hook::post(&a, &secret(), &p, Duration::from_secs(5)).await }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(()) => ok += 1,
            Err(e) => eprintln!("QA concurrent resend error: {e:?}"),
        }
    }
    let mut n = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        n += 1;
    }
    eprintln!("QA concurrent resends: {ok} ok, {n} delivered");
    assert_eq!(ok, 40);
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_hook_client_reports_503_and_redelivers() {
    let (addr, mut rx) = hub(1).await;
    let a = addr.to_string();
    let first = sample();
    let second = sample();
    assert_eq!(hook::post(&a, &secret(), &first, Duration::from_secs(2)).await, Ok(()));
    assert_eq!(hook::post(&a, &secret(), &second, Duration::from_secs(2)).await, Err(PostError::Status(503)));
    assert_eq!(rx.recv().await, Some(first));
    assert_eq!(hook::post(&a, &secret(), &second, Duration::from_secs(2)).await, Ok(()));
    assert_eq!(rx.recv().await, Some(second));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_hook_client_status_line_split_across_segments_is_ok() {
    let l = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await.unwrap();
    let a = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = s.read(&mut buf).await;
        s.write_all(b"HTTP/1.1 20").await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        s.write_all(b"4 No Content\r\n\r\n").await.unwrap();
        let _ = s.shutdown().await;
        let _ = s.read_to_end(&mut buf).await;
    });
    assert_eq!(hook::post(&a, &secret(), &sample(), Duration::from_secs(2)).await, Ok(()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_hook_timing_budget() {
    // one POST to a live hub: well under the SessionEnd budget
    let (addr, mut rx) = hub(8).await;
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let a = addr.to_string();
    let mut worst = Duration::ZERO;
    for _ in 0..20 {
        let t = Instant::now();
        hook::post(&a, &secret(), &sample(), Duration::from_secs(1)).await.unwrap();
        worst = worst.max(t.elapsed());
    }
    eprintln!("QA hook worst of 20 = {worst:?}");
    assert!(worst < Duration::from_millis(300));
}

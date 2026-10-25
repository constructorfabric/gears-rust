//! Security regression tests: request smuggling, response desync and
//! HTTP/2 resource-exhaustion attacks from a malicious upstream.
//!
//! Each test drives the engine against a raw-socket upstream that writes
//! exact bytes, so behaviour does not depend on a well-behaved server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    clippy::use_debug
)]

mod support;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use http::Request;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use toolkit_http_proxy::{
    ErrorKind, ProtocolPolicy, ProxyClient, Target, Transport, empty_body, full_body, stream_body,
};

// ---------------------------------------------------------------------------
// Raw upstream helpers
// ---------------------------------------------------------------------------

/// Accept loop handing each connection (with its 1-based index) to `f`.
async fn raw<F, Fut>(f: F) -> (SocketAddr, Arc<AtomicUsize>)
where
    F: Fn(TcpStream, usize) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = listener.accept().await else {
                continue;
            };
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::spawn(f.clone()(s, n));
        }
    });
    (addr, count)
}

/// Read one HTTP/1.1 request head (up to and including the blank line).
async fn read_head(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Read everything the client sends for up to `wait`.
async fn drain(s: &mut TcpStream, wait: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(Ok(n)) = tokio::time::timeout(wait, s.read(&mut buf)).await {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

fn plain(addr: SocketAddr) -> Target {
    Target::new(addr, "localhost", Transport::Plain)
}

fn client() -> ProxyClient {
    support::client()
}

fn client_with_timeout(read: Duration) -> ProxyClient {
    let mut cfg = support::test_config();
    cfg.read_timeout = Some(read);
    ProxyClient::new(cfg).unwrap()
}

// ---------------------------------------------------------------------------
// Outbound framing (CVE-2026-2835 class, 0.CL / Expect desync, h2c)
// ---------------------------------------------------------------------------

/// Upstream that records every byte of the first connection and answers 200.
async fn capture() -> (SocketAddr, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (addr, _) = raw(move |mut s, _| {
        let tx = tx.clone();
        async move {
            let bytes = drain(&mut s, Duration::from_millis(300)).await;
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
            let _ = tx.send(bytes);
        }
    })
    .await;
    (addr, rx)
}

#[tokio::test]
async fn ambiguous_inbound_framing_is_never_forwarded() {
    let (addr, mut rx) = capture().await;
    let req = Request::post("/submit")
        .header("content-length", "29")
        .header("transfer-encoding", "identity, chunked")
        .header("expect", "100-continue")
        .header(
            "connection",
            "keep-alive, x-secret, Upgrade, HTTP2-Settings",
        )
        .header("x-secret", "internal")
        .header("upgrade", "h2c")
        .header("http2-settings", "AAMAAABkAARAAAAAAAIAAAAA")
        .header("keep-alive", "timeout=5")
        .header("x-end-to-end", "kept")
        .body(full_body(Bytes::from_static(b"hello")))
        .unwrap();
    let resp = client().send(&plain(addr), req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let wire = String::from_utf8(rx.recv().await.unwrap()).unwrap();
    let (head, body) = wire.split_once("\r\n\r\n").expect("complete head");
    let head = head.to_ascii_lowercase();
    assert!(head.starts_with("post /submit http/1.1\r\n"), "{head}");
    assert!(head.contains("\r\ncontent-length: 5"), "{head}");
    for banned in [
        "transfer-encoding",
        "expect",
        "x-secret",
        "upgrade",
        "http2-settings",
        "keep-alive",
        "connection: keep-alive",
    ] {
        assert!(!head.contains(banned), "{banned} leaked: {head}");
    }
    assert!(head.contains("x-end-to-end: kept"));
    assert_eq!(body, "hello", "exactly one request with exactly its body");
}

#[tokio::test]
async fn streaming_bodies_use_chunked_framing_only() {
    let (addr, mut rx) = capture().await;
    let chunks = stream::iter(vec![
        Ok::<_, std::io::Error>(Bytes::from_static(b"abc")),
        Ok(Bytes::from_static(b"def")),
    ]);
    let req = Request::post("/up")
        .header("content-length", "3")
        .body(stream_body(chunks))
        .unwrap();
    client().send(&plain(addr), req).await.unwrap();
    let wire = String::from_utf8(rx.recv().await.unwrap()).unwrap();
    let lower = wire.to_ascii_lowercase();
    assert!(lower.contains("transfer-encoding: chunked"), "{wire}");
    assert!(!lower.contains("content-length"), "{wire}");
    assert!(
        wire.ends_with("3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n"),
        "{wire:?}"
    );
}

#[tokio::test]
async fn percent_encoded_crlf_reaches_the_wire_encoded() {
    let (addr, mut rx) = capture().await;
    let req = Request::get("/a%0d%0aInjected:%20x%00")
        .body(empty_body())
        .unwrap();
    client().send(&plain(addr), req).await.unwrap();
    let wire = String::from_utf8(rx.recv().await.unwrap()).unwrap();
    let request_line = wire.lines().next().unwrap();
    assert_eq!(request_line, "GET /a%0d%0aInjected:%20x%00 HTTP/1.1");
    assert!(!wire.contains("\r\nInjected"));
}

// ---------------------------------------------------------------------------
// Premature upgrade (CVE-2026-2833 / RUSTSEC-2026-0033 class)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upgrade_without_101_tunnels_nothing() {
    let extra = Arc::new(AtomicUsize::new(0));
    let seen = extra.clone();
    let (addr, conns) = raw(move |mut s, _| {
        let seen = seen.clone();
        async move {
            if read_head(&mut s).await.is_none() {
                return;
            }
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nno")
                .await;
            // Anything the client sends after this would be a smuggled,
            // already-"upgraded" byte stream.
            seen.fetch_add(
                drain(&mut s, Duration::from_millis(300)).await.len(),
                Ordering::SeqCst,
            );
        }
    })
    .await;
    let c = client();
    let req = Request::get("/ws")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .body(empty_body())
        .unwrap();
    let outcome = c.upgrade(&plain(addr), req).await.unwrap();
    let toolkit_http_proxy::UpgradeOutcome::Rejected(resp) = outcome else {
        panic!("a 200 must not switch protocols");
    };
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes(), "no");

    // The rejected handshake connection is never pooled.
    c.send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(conns.load(Ordering::SeqCst), 2);
    assert_eq!(
        extra.load(Ordering::SeqCst),
        0,
        "no bytes after the rejected upgrade"
    );
}

#[tokio::test]
async fn bogus_101_is_rejected() {
    let (addr, _) = raw(|mut s, _| async move {
        read_head(&mut s).await;
        let _ = s
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\nConnection: Upgrade\r\n\r\n",
            )
            .await;
        drain(&mut s, Duration::from_millis(300)).await;
    })
    .await;
    let req = Request::get("/ws")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .body(empty_body())
        .unwrap();
    let err = client().upgrade(&plain(addr), req).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Protocol);
}

// ---------------------------------------------------------------------------
// Malformed or hostile HTTP/1.1 responses
// ---------------------------------------------------------------------------

/// Upstream answering every request on a connection with `responses[i]`
/// (the last one repeats).
async fn scripted(responses: &'static [&'static [u8]]) -> (SocketAddr, Arc<AtomicUsize>) {
    raw(move |mut s, _| async move {
        let mut i = 0;
        while read_head(&mut s).await.is_some() {
            let r = responses[i.min(responses.len() - 1)];
            if s.write_all(r).await.is_err() {
                return;
            }
            i += 1;
        }
    })
    .await
}

async fn head_error(response: &'static [u8]) -> ErrorKind {
    let (addr, _) = scripted(Box::leak(Box::new([response]))).await;
    client()
        .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
        .await
        .expect_err("malformed response must fail")
        .kind()
}

#[tokio::test]
async fn malformed_response_heads_are_rejected() {
    let too_many: &'static [u8] = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n",
            "x-h: v\r\n".repeat(200)
        )
        .into_bytes()
        .into_boxed_slice(),
    );
    let too_big: &'static [u8] = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nx-big: {}\r\nContent-Length: 0\r\n\r\n",
            // hyper checks the limit between reads, so a head that arrives
            // in one read may exceed it by up to the buffer growth step;
            // memory stays bounded (~2x). Far beyond the limit must fail.
            "a".repeat(2 * 1024 * 1024)
        )
        .into_bytes()
        .into_boxed_slice(),
    );
    let cases: [(&str, &'static [u8]); 6] = [
        (
            "conflicting content-length",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\ngood",
        ),
        (
            "space before colon",
            b"HTTP/1.1 200 OK\r\nContent-Length : 4\r\n\r\ngood",
        ),
        (
            "obs-fold",
            b"HTTP/1.1 200 OK\r\nX-A: a\r\n b\r\nContent-Length: 4\r\n\r\ngood",
        ),
        (
            "bad status line",
            b"HTTP/1.1 2OO OK\r\nContent-Length: 4\r\n\r\ngood",
        ),
        ("too many headers", too_many),
        ("oversized head", too_big),
    ];
    for (name, bytes) in cases {
        let kind = head_error(bytes).await;
        assert!(
            matches!(kind, ErrorKind::Protocol | ErrorKind::ConnectionClosed),
            "{name}: {kind:?}"
        );
    }
}

#[tokio::test]
async fn malformed_chunked_bodies_fail_and_are_not_pooled() {
    let cases: [&'static [u8]; 3] = [
        // Bare LF inside a chunk extension ("funky chunks").
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;ext\nabcd\r\n0\r\n\r\n",
        // LF-only chunk-size line.
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\nabcd\r\n0\r\n\r\n",
        // Non-hex chunk size.
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabcd\r\n0\r\n\r\n",
    ];
    for bytes in cases {
        let (addr, conns) = scripted(Box::leak(Box::new([
            bytes,
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nnext",
        ])))
        .await;
        let c = client();
        let resp = c
            .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
            .await
            .unwrap();
        assert!(
            resp.into_body().collect().await.is_err(),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
        let resp = c
            .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
            .await
            .unwrap();
        let _ = resp.into_body().collect().await;
        assert_eq!(
            conns.load(Ordering::SeqCst),
            2,
            "a failed connection is never reused"
        );
    }
}

#[tokio::test]
async fn injected_pipelined_response_is_never_served() {
    // First answer carries a second, unsolicited response behind it.
    let (addr, _) = scripted(&[
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ngoodHTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nevil",
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nnext",
    ])
    .await;
    let c = client();
    let get = || Request::get("/").body(empty_body()).unwrap();
    let first = c.send(&plain(addr), get()).await.unwrap();
    assert_eq!(
        first.into_body().collect().await.unwrap().to_bytes(),
        "good"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    match c.send(&plain(addr), get()).await {
        Ok(second) => {
            let body = second
                .into_body()
                .collect()
                .await
                .map(http_body_util::Collected::to_bytes);
            assert_ne!(
                body.ok().as_deref(),
                Some(&b"evil"[..]),
                "desynced response served"
            );
        }
        Err(e) => assert_ne!(e.kind(), ErrorKind::InvalidRequest),
    }
}

#[tokio::test]
async fn chunked_with_content_length_uses_chunked_framing() {
    let (addr, _) = scripted(&[
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ngood\r\n0\r\n\r\n",
    ])
    .await;
    let resp = client()
        .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap();
    assert!(resp.headers().get("transfer-encoding").is_none());
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes(), "good");
}

// ---------------------------------------------------------------------------
// Malicious HTTP/2 upstream (h2c prior knowledge)
// ---------------------------------------------------------------------------

const PREFACE_LEN: usize = 24;
const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const SETTINGS: u8 = 0x4;
const WINDOW_UPDATE: u8 = 0x8;
const CONTINUATION: u8 = 0x9;
const RST_STREAM: u8 = 0x3;
const END_STREAM: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const ACK: u8 = 0x1;

fn frame(ty: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).unwrap();
    let mut f = Vec::with_capacity(9 + payload.len());
    f.extend_from_slice(&len.to_be_bytes()[1..]);
    f.push(ty);
    f.push(flags);
    f.extend_from_slice(&stream.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// Run the HTTP/2 handshake as a server, wait for the client's first
/// HEADERS, then hand over to `attack`.
async fn h2_server<F, Fut>(settings: &'static [u8], attack: F) -> SocketAddr
where
    F: Fn(TcpStream) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    raw(move |mut s, _| {
        let attack = attack.clone();
        async move {
            let mut preface = [0u8; PREFACE_LEN];
            if s.read_exact(&mut preface).await.is_err() {
                return;
            }
            let _ = s.write_all(&frame(SETTINGS, 0, 0, settings)).await;
            loop {
                let mut h = [0u8; 9];
                if s.read_exact(&mut h).await.is_err() {
                    return;
                }
                let len = usize::from(h[0]) << 16 | usize::from(h[1]) << 8 | usize::from(h[2]);
                let mut payload = vec![0u8; len];
                if s.read_exact(&mut payload).await.is_err() {
                    return;
                }
                match h[3] {
                    SETTINGS if h[4] & ACK == 0 => {
                        let _ = s.write_all(&frame(SETTINGS, ACK, 0, &[])).await;
                    }
                    HEADERS => break,
                    _ => {}
                }
            }
            attack(s).await;
        }
    })
    .await
    .0
}

fn h2_target(addr: SocketAddr) -> Target {
    plain(addr).with_policy(ProtocolPolicy::Http2Only)
}

/// Read frames until the client resets stream 1. `false` if the connection
/// ends first.
async fn reset_of_stream_1(s: &mut TcpStream) -> bool {
    loop {
        let mut h = [0u8; 9];
        if s.read_exact(&mut h).await.is_err() {
            return false;
        }
        let len = usize::from(h[0]) << 16 | usize::from(h[1]) << 8 | usize::from(h[2]);
        let mut payload = vec![0u8; len];
        if s.read_exact(&mut payload).await.is_err() {
            return false;
        }
        let stream = u32::from_be_bytes([h[5], h[6], h[7], h[8]]) & 0x7fff_ffff;
        if h[3] == RST_STREAM && stream == 1 {
            return true;
        }
    }
}

async fn within<T>(limit: Duration, fut: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let out = tokio::time::timeout(limit, fut)
        .await
        .unwrap_or_else(|_| panic!("attack not contained within {limit:?}"));
    assert!(started.elapsed() <= limit);
    out
}

#[tokio::test]
async fn hpack_indexed_reference_bomb_is_contained() {
    // CVE-2026-49975: one tiny dynamic-table entry referenced 200k times.
    let addr = h2_server(&[], |mut s| async move {
        let mut block = vec![0x88, 0x40, 0x01, b'a', 0x01, b'b'];
        block.extend(std::iter::repeat_n(0xBE, 200_000));
        let mut chunks = block.chunks(16_384).peekable();
        let first = chunks.next().unwrap();
        let mut out = frame(HEADERS, 0, 1, first);
        while let Some(c) = chunks.next() {
            let flags = if chunks.peek().is_none() {
                END_HEADERS
            } else {
                0
            };
            out.extend(frame(CONTINUATION, flags, 1, c));
        }
        let _ = s.write_all(&out).await;
        drain(&mut s, Duration::from_secs(3)).await;
    })
    .await;
    let res = within(
        Duration::from_secs(3),
        client().send(
            &h2_target(addr),
            Request::get("/").body(empty_body()).unwrap(),
        ),
    )
    .await;
    let err = res.expect_err("bomb must not produce a response");
    assert!(
        matches!(
            err.kind(),
            ErrorKind::Http2 | ErrorKind::Protocol | ErrorKind::ConnectionClosed
        ),
        "{err}"
    );
}

#[tokio::test]
async fn continuation_flood_is_contained() {
    let addr = h2_server(&[], |mut s| async move {
        let _ = s.write_all(&frame(HEADERS, 0, 1, &[0x88])).await;
        let junk: Vec<u8> = std::iter::repeat_n([0x00, 0x01, b'x', 0x01, b'y'], 3000)
            .flatten()
            .collect();
        let cont = frame(CONTINUATION, 0, 1, &junk);
        for _ in 0..10_000 {
            if s.write_all(&cont).await.is_err() {
                return;
            }
        }
    })
    .await;
    let res = within(
        Duration::from_secs(3),
        client().send(
            &h2_target(addr),
            Request::get("/").body(empty_body()).unwrap(),
        ),
    )
    .await;
    assert!(res.is_err(), "flood must not produce a response");
}

#[tokio::test]
async fn zero_window_slowloris_hits_the_read_timeout() {
    // SETTINGS_INITIAL_WINDOW_SIZE = 0, then 1-byte WINDOW_UPDATEs forever.
    let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], |mut s| async move {
        let update = frame(WINDOW_UPDATE, 0, 1, &1u32.to_be_bytes());
        loop {
            if s.write_all(&update).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let c = client_with_timeout(Duration::from_millis(500));
    let req = Request::post("/")
        .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
        .unwrap();
    let err = within(Duration::from_secs(3), c.send(&h2_target(addr), req))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadTimeout, "{err}");
}

#[tokio::test]
async fn empty_data_frame_flood_is_contained() {
    // RUSTSEC-2026-0258: unbounded empty DATA frames.
    let addr = h2_server(&[], |mut s| async move {
        let _ = s.write_all(&frame(HEADERS, END_HEADERS, 1, &[0x88])).await;
        let empty = frame(DATA, 0, 1, &[]);
        let burst: Vec<u8> = std::iter::repeat_n(empty, 1000).flatten().collect();
        for _ in 0..200 {
            if s.write_all(&burst).await.is_err() {
                return;
            }
        }
        let _ = s.write_all(&frame(DATA, END_STREAM, 1, b"ok")).await;
        drain(&mut s, Duration::from_secs(2)).await;
    })
    .await;
    let c = client_with_timeout(Duration::from_secs(2));
    within(Duration::from_secs(5), async {
        if let Ok(resp) = c
            .send(
                &h2_target(addr),
                Request::get("/").body(empty_body()).unwrap(),
            )
            .await
        {
            // Either the body completes or the flood is cut off; both are
            // fine as long as it ends promptly.
            let _ = resp.into_body().collect().await;
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// Review regressions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ambiguous_response_framing_drops_length_and_connection() {
    let (addr, conns) = scripted(&[
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ngood\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nnext",
    ])
    .await;
    let c = client();
    let get = || Request::get("/").body(empty_body()).unwrap();
    let resp = c.send(&plain(addr), get()).await.unwrap();
    assert!(
        resp.headers().get("content-length").is_none(),
        "a lying length must not be relayed"
    );
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes(), "good");
    let resp = c.send(&plain(addr), get()).await.unwrap();
    let _ = resp.into_body().collect().await;
    assert_eq!(
        conns.load(Ordering::SeqCst),
        2,
        "never reuse an ambiguously framed connection"
    );
}

#[tokio::test]
async fn early_h2_response_with_zero_window_upload_is_torn_down() {
    // The upstream answers immediately but never opens the window, so the
    // upload would hang forever after the response. The engine must reset
    // the stream (and only the stream).
    let reset = Arc::new(tokio::sync::Notify::new());
    let notify = reset.clone();
    let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], move |mut s| {
        let notify = notify.clone();
        async move {
            let _ = s
                .write_all(&frame(HEADERS, END_HEADERS | END_STREAM, 1, &[0x88]))
                .await;
            if reset_of_stream_1(&mut s).await {
                notify.notify_one();
            }
        }
    })
    .await;
    let mut cfg = support::test_config();
    cfg.write_timeout = Some(Duration::from_millis(300));
    let c = ProxyClient::new(cfg).unwrap();
    let req = Request::post("/")
        .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
        .unwrap();
    let resp = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);
    within(Duration::from_secs(3), reset.notified()).await;
}

#[tokio::test]
async fn h2_upload_stall_is_reported_as_a_write_timeout() {
    // The window never opens and no response comes: the upload's stall
    // budget runs out first and that, not a protocol error, is the cause.
    let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], |mut s| async move {
        drain(&mut s, Duration::from_secs(3)).await;
    })
    .await;
    let mut cfg = support::test_config();
    cfg.write_timeout = Some(Duration::from_millis(300));
    cfg.read_timeout = Some(Duration::from_secs(5));
    let c = ProxyClient::new(cfg).unwrap();
    // Larger than the default window the client may use before it sees the
    // upstream's SETTINGS.
    let req = Request::post("/")
        .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
        .unwrap();
    let err = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WriteTimeout, "{err}");
}

/// Upstream that never answers and reports whether the client reset
/// stream 1.
async fn silent_h2_upstream() -> (SocketAddr, Arc<tokio::sync::Notify>) {
    let reset = Arc::new(tokio::sync::Notify::new());
    let notify = reset.clone();
    let addr = h2_server(&[], move |mut s| {
        let notify = notify.clone();
        async move {
            if reset_of_stream_1(&mut s).await {
                notify.notify_one();
            }
        }
    })
    .await;
    (addr, reset)
}

/// A request body that yields one chunk and then never ends.
fn endless_upload() -> toolkit_http_proxy::ProxyBody {
    let chunks =
        stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"x"))]).chain(stream::pending());
    stream_body(chunks)
}

#[tokio::test]
async fn h2_head_timeout_resets_the_stream_and_stops_the_upload() {
    let (addr, reset) = silent_h2_upstream().await;
    let c = client_with_timeout(Duration::from_millis(300));
    let req = Request::post("/").body(endless_upload()).unwrap();
    let err = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadTimeout, "{err}");
    within(Duration::from_secs(2), reset.notified()).await;
}

#[tokio::test]
async fn dropping_an_h2_send_resets_the_stream_and_stops_the_upload() {
    let (addr, reset) = silent_h2_upstream().await;
    let c = client();
    let req = Request::post("/").body(endless_upload()).unwrap();
    let target = h2_target(addr);
    let send = c.send(&target, req);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), send)
            .await
            .is_err(),
        "the upstream never answers"
    );
    within(Duration::from_secs(2), reset.notified()).await;
}

/// HPACK: `content-length` (static index 28) literal, not indexed.
fn content_length(value: u8) -> [u8; 4] {
    [0x0F, 0x0D, 0x01, value]
}

#[tokio::test]
async fn malformed_h2_response_heads_are_rejected() {
    // Conflicting content-length values, and a head without `:status`.
    let duplicate: Vec<u8> = [&[0x88][..], &content_length(b'1'), &content_length(b'2')].concat();
    let no_status = content_length(b'0').to_vec();
    for (block, data) in [(duplicate, &b"x"[..]), (no_status, &b""[..])] {
        let addr = h2_server(&[], move |mut s| {
            let block = block.clone();
            async move {
                let mut out = frame(HEADERS, END_HEADERS, 1, &block);
                out.extend(frame(DATA, END_STREAM, 1, data));
                let _ = s.write_all(&out).await;
                drain(&mut s, Duration::from_secs(2)).await;
            }
        })
        .await;
        let res = within(
            Duration::from_secs(2),
            client().send(
                &h2_target(addr),
                Request::get("/").body(empty_body()).unwrap(),
            ),
        )
        .await;
        let err = res.expect_err("a malformed head must not become a response");
        assert!(
            matches!(err.kind(), ErrorKind::Http2 | ErrorKind::Protocol),
            "{err}"
        );
    }
}

#[tokio::test]
async fn complete_h2_response_survives_an_upload_stall() {
    // RFC 9113 §8.1: a complete response before the request finished is
    // valid. A closed upload window must not turn it into an error, however
    // long the caller holds it before reading.
    for with_body in [false, true] {
        let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], move |mut s| async move {
            let out = if with_body {
                let mut out = frame(HEADERS, END_HEADERS, 1, &[0x88]);
                out.extend(frame(DATA, END_STREAM, 1, b"ok"));
                out
            } else {
                frame(HEADERS, END_HEADERS | END_STREAM, 1, &[0x88])
            };
            let _ = s.write_all(&out).await;
            drain(&mut s, Duration::from_secs(3)).await;
        })
        .await;
        let mut cfg = support::test_config();
        cfg.write_timeout = Some(Duration::from_millis(200));
        let c = ProxyClient::new(cfg).unwrap();
        let req = Request::post("/")
            .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
            .unwrap();
        let resp = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let body = resp.into_body().collect().await.expect("complete response");
        let expected: &[u8] = if with_body { b"ok" } else { b"" };
        assert_eq!(body.to_bytes(), expected);
    }
}

#[tokio::test]
async fn h2_upload_stall_after_the_head_still_fails_the_exchange() {
    // Zero window, a head without END_STREAM, then a trickle of DATA that
    // keeps the read timeout from ever firing: the stalled upload still
    // bounds the exchange.
    let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], |mut s| async move {
        let _ = s.write_all(&frame(HEADERS, END_HEADERS, 1, &[0x88])).await;
        for _ in 0..100 {
            if s.write_all(&frame(DATA, 0, 1, b"x")).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let mut cfg = support::test_config();
    cfg.write_timeout = Some(Duration::from_millis(300));
    cfg.read_timeout = Some(Duration::from_secs(5));
    let c = ProxyClient::new(cfg).unwrap();
    let req = Request::post("/")
        .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
        .unwrap();
    let resp = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
        .await
        .unwrap();
    let err = within(Duration::from_secs(3), resp.into_body().collect())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WriteTimeout, "{err}");
}

/// A well-behaved h2 server that never opens its upload window and streams
/// response DATA as fast as flow control allows, forever.
async fn flooding_h2_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut conn) = h2::server::Builder::new()
                    .initial_window_size(0)
                    .handshake::<_, Bytes>(sock)
                    .await
                else {
                    return;
                };
                let Some(Ok((_req, mut respond))) = conn.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let Ok(mut out) = respond.send_response(http::Response::new(()), false) else {
                        return;
                    };
                    loop {
                        out.reserve_capacity(16 * 1024);
                        match std::future::poll_fn(|cx| out.poll_capacity(cx)).await {
                            Some(Ok(0)) => {}
                            Some(Ok(n)) => {
                                if out.send_data(Bytes::from(vec![b'x'; n]), false).is_err() {
                                    return;
                                }
                            }
                            _ => return,
                        }
                    }
                });
                while conn.accept().await.is_some() {}
            });
        }
    });
    addr
}

#[tokio::test]
async fn h2_upload_stall_is_not_hidden_by_a_response_flood() {
    // The upstream keeps the receive queue full: draining after the abort
    // must still end, bounded by the window the engine stops replenishing.
    let addr = flooding_h2_upstream().await;
    let mut cfg = support::test_config();
    cfg.write_timeout = Some(Duration::from_millis(300));
    cfg.read_timeout = Some(Duration::from_secs(5));
    let window = cfg.h2.initial_stream_window_size as usize;
    let c = ProxyClient::new(cfg).unwrap();
    let req = Request::post("/")
        .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
        .unwrap();
    let resp = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let started = Instant::now();
    let mut after_abort = 0usize;
    let err = within(Duration::from_secs(5), async {
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if started.elapsed() > Duration::from_millis(400) {
                        after_abort += frame.into_data().map_or(0, |d| d.len());
                    }
                    // Consume slower than the upstream sends, so the
                    // receive queue never runs empty on its own.
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Some(Err(e)) => break e,
                None => panic!("the flood never ends"),
            }
        }
    })
    .await;
    assert_eq!(err.kind(), ErrorKind::WriteTimeout, "{err}");
    assert!(
        after_abort <= window,
        "{after_abort} bytes drained after the abort"
    );
}

#[tokio::test]
async fn completed_h2_response_stops_the_upload_while_still_held() {
    // Once the response is complete the upload is pointless: it must stop
    // even though the caller keeps the response (or its body) alive.
    for with_body in [false, true] {
        let reset = Arc::new(tokio::sync::Notify::new());
        let notify = reset.clone();
        let addr = h2_server(&[0x00, 0x04, 0, 0, 0, 0], move |mut s| {
            let notify = notify.clone();
            async move {
                let out = if with_body {
                    let mut out = frame(HEADERS, END_HEADERS, 1, &[0x88]);
                    out.extend(frame(DATA, END_STREAM, 1, b"ok"));
                    out
                } else {
                    frame(HEADERS, END_HEADERS | END_STREAM, 1, &[0x88])
                };
                let _ = s.write_all(&out).await;
                if reset_of_stream_1(&mut s).await {
                    notify.notify_one();
                }
            }
        })
        .await;
        let mut cfg = support::test_config();
        cfg.write_timeout = None;
        let c = ProxyClient::new(cfg).unwrap();
        let req = Request::post("/")
            .body(full_body(Bytes::from(vec![b'x'; 256 * 1024])))
            .unwrap();
        let resp = within(Duration::from_secs(2), c.send(&h2_target(addr), req))
            .await
            .unwrap();
        let mut body = resp.into_body();
        if with_body {
            let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
            assert_eq!(data, "ok");
            assert!(http_body::Body::is_end_stream(&body));
        }
        within(Duration::from_secs(2), reset.notified()).await;
        drop(body);
    }
}

#[tokio::test]
async fn h2_body_cut_by_a_reset_never_reports_its_end() {
    // A truncated body must not look complete: consumers that trust
    // `is_end_stream` would otherwise terminate it cleanly downstream.
    let addr = h2_server(&[], |mut s| async move {
        let mut out = frame(HEADERS, END_HEADERS, 1, &[0x88]);
        out.extend(frame(DATA, 0, 1, b"part"));
        out.extend(frame(RST_STREAM, 0, 1, &2u32.to_be_bytes()));
        let _ = s.write_all(&out).await;
        drain(&mut s, Duration::from_secs(2)).await;
    })
    .await;
    let resp = within(
        Duration::from_secs(2),
        client().send(
            &h2_target(addr),
            Request::get("/").body(empty_body()).unwrap(),
        ),
    )
    .await
    .unwrap();
    let mut body = resp.into_body();
    let first = body.frame().await.unwrap().unwrap();
    assert_eq!(first.into_data().unwrap(), "part");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!http_body::Body::is_end_stream(&body));
    let next = body.frame().await;
    assert!(matches!(next, Some(Err(_))), "{next:?}");
}

/// Upstream that answers with `response` as soon as it has the request
/// head, while the request body is still open, then reads until the client
/// closes the (first) connection and notifies.
async fn early_answer(response: &'static [u8]) -> (SocketAddr, Arc<tokio::sync::Notify>) {
    let closed = Arc::new(tokio::sync::Notify::new());
    let notify = closed.clone();
    let (addr, _) = raw(move |mut s, n| {
        let notify = notify.clone();
        async move {
            if read_head(&mut s).await.is_none() {
                return;
            }
            let _ = s.write_all(response).await;
            let mut buf = [0u8; 4096];
            while let Ok(n) = s.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
            if n == 1 {
                notify.notify_one();
            }
        }
    })
    .await;
    (addr, closed)
}

const EARLY_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

async fn send_endless_upload(c: &ProxyClient, addr: SocketAddr) -> Bytes {
    let req = Request::post("/").body(endless_upload()).unwrap();
    let resp = within(Duration::from_secs(2), c.send(&plain(addr), req))
        .await
        .unwrap();
    resp.into_body().collect().await.unwrap().to_bytes()
}

#[tokio::test]
async fn h1_connection_with_an_unfinished_upload_closes_when_the_pool_lets_go() {
    // The upstream answers while the request body is still open. Once the
    // pool discards the connection (pooling disabled here, or the client
    // going away), it must close instead of polling the upload forever.
    for disable_pooling in [true, false] {
        let (addr, closed) = early_answer(EARLY_OK).await;
        let mut cfg = support::test_config();
        if disable_pooling {
            cfg.pool.max_idle_per_host = 0;
        }
        let c = ProxyClient::new(cfg).unwrap();
        assert_eq!(send_endless_upload(&c, addr).await, "ok");
        if !disable_pooling {
            drop(c);
        }
        within(Duration::from_secs(2), closed.notified()).await;
    }
}

#[tokio::test]
async fn h1_ambiguous_response_to_an_unfinished_upload_closes_the_connection() {
    // TE + CL: never pooled; once the response is consumed the connection
    // must close rather than keep polling the upload.
    let (addr, closed) = early_answer(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
    )
    .await;
    let c = client();
    assert_eq!(send_endless_upload(&c, addr).await, "ok");
    within(Duration::from_secs(2), closed.notified()).await;
}

#[tokio::test]
async fn cancelled_h1_checkout_closes_the_connection_it_took() {
    // The pooled connection is still uploading, so checkout waits for it to
    // become ready; cancelling that wait must not orphan it.
    let (addr, closed) = early_answer(EARLY_OK).await;
    let c = client();
    assert_eq!(send_endless_upload(&c, addr).await, "ok");
    let target = plain(addr);
    let get = c.send(&target, Request::get("/").body(empty_body()).unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), get)
            .await
            .is_err(),
        "checkout is still waiting for readiness"
    );
    within(Duration::from_secs(2), closed.notified()).await;
}

#[tokio::test]
async fn early_h1_response_with_unread_upload_severs_the_connection() {
    // The upstream answers right away and never reads the body: once the
    // socket buffers fill, the upload's writes stay pending.
    let closed = Arc::new(tokio::sync::Notify::new());
    let notify = closed.clone();
    let (addr, _) = raw(move |mut s, _| {
        let notify = notify.clone();
        async move {
            if read_head(&mut s).await.is_none() {
                return;
            }
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            // Now drain: the client must have given up on the connection.
            let mut buf = vec![0u8; 1 << 20];
            loop {
                match tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await {
                    Ok(Ok(0) | Err(_)) => {
                        notify.notify_one();
                        return;
                    }
                    Ok(Ok(_)) => {}
                    Err(_) => return,
                }
            }
        }
    })
    .await;
    let mut cfg = support::test_config();
    cfg.write_timeout = Some(Duration::from_millis(300));
    let c = ProxyClient::new(cfg).unwrap();
    let chunk = Bytes::from(vec![b'x'; 1 << 20]);
    let body = stream::iter((0..256).map(move |_| Ok::<_, std::io::Error>(chunk.clone())));
    let req = Request::post("/").body(stream_body(body)).unwrap();
    let resp = within(Duration::from_secs(3), c.send(&plain(addr), req))
        .await
        .unwrap();
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes(), "ok");
    within(Duration::from_secs(5), closed.notified()).await;
}

#[tokio::test]
async fn http2_only_requires_negotiated_h2() {
    // An HTTP/1.1-only server rejects the `h2` offer during the handshake.
    let up = support::echo(support::Wire::TlsH1).await;
    let t = up
        .target(support::Wire::TlsH1)
        .with_policy(ProtocolPolicy::Http2Only);
    let err = client()
        .send(&t, Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TlsHandshake, "{err}");

    // A server without ALPN completes the handshake; the engine refuses to
    // fall back to HTTP/1.1.
    let up = support::echo(support::Wire::TlsNoAlpn).await;
    let t = up
        .target(support::Wire::TlsNoAlpn)
        .with_policy(ProtocolPolicy::Http2Only);
    let err = client()
        .send(&t, Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Protocol, "{err}");
}

#[tokio::test]
async fn unknown_length_get_bodies_are_framed_not_dropped() {
    let (addr, mut rx) = capture().await;
    let chunks = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"query"))]);
    let req = Request::get("/search").body(stream_body(chunks)).unwrap();
    client().send(&plain(addr), req).await.unwrap();
    let wire = String::from_utf8(rx.recv().await.unwrap()).unwrap();
    assert!(
        wire.to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "{wire}"
    );
    assert!(wire.ends_with("5\r\nquery\r\n0\r\n\r\n"), "{wire:?}");
}

#[tokio::test]
async fn body_idle_timeout_is_terminal_and_not_pooled() {
    let (addr, conns) = raw(|mut s, n| async move {
        if read_head(&mut s).await.is_none() {
            return;
        }
        if n == 1 {
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nfirst")
                .await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            // Resumes after the client timed out; must not revive the exchange.
            let _ = s.write_all(b"later").await;
            drain(&mut s, Duration::from_secs(1)).await;
        } else {
            let _ = s
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nnext")
                .await;
            drain(&mut s, Duration::from_millis(300)).await;
        }
    })
    .await;
    let c = client_with_timeout(Duration::from_millis(200));
    let resp = c
        .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap();
    let mut body = resp.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(&first[..], b"first");
    assert_eq!(
        body.frame().await.unwrap().unwrap_err().kind(),
        ErrorKind::ReadTimeout
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(body.frame().await.is_none(), "a timed-out body stays ended");
    drop(body);
    let resp = c
        .send(&plain(addr), Request::get("/").body(empty_body()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes(), "next");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

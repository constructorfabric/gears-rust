//! HTTP/1.1 upstream edge cases: response framing, keep-alive decisions,
//! interim responses, bodiless statuses, early closes and upgrades.
//!
//! Each case runs against a scripted raw-socket upstream, so the bytes on
//! the wire are exact. Requests beyond a connection's script are answered
//! with `conn=<n>`, which shows whether the engine reused the connection.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    clippy::use_debug
)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Body as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use toolkit_http_proxy::{ErrorKind, ProxyClient, ResponseBody, Target, Transport, empty_body};

/// One step of a scripted upstream connection.
#[derive(Clone, Copy)]
enum Step {
    /// Read one request head.
    Read,
    /// Write these bytes.
    Write(&'static [u8]),
    /// Pause, so the next write arrives in a separate read.
    Pause,
    /// Close the connection.
    Close,
}

use Step::{Close, Pause, Read, Write};

/// Upstream whose `n`-th connection plays `scripts[n - 1]` (the last script
/// repeats), then answers every further request with `conn=<n>`.
async fn scripted(scripts: &[&'static [Step]]) -> (Target, Arc<AtomicUsize>) {
    let scripts: Arc<Vec<&'static [Step]>> = Arc::new(scripts.to_vec());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let conns = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                continue;
            };
            // Each write leaves at once, so a `Pause` really splits reads.
            let _ = s.set_nodelay(true);
            let n = conns.fetch_add(1, Ordering::SeqCst) + 1;
            let script = scripts[(n - 1).min(scripts.len() - 1)];
            tokio::spawn(async move {
                for step in script {
                    match step {
                        Read => {
                            if read_head(&mut s).await.is_none() {
                                return;
                            }
                        }
                        Write(bytes) => {
                            if s.write_all(bytes).await.is_err() {
                                return;
                            }
                        }
                        Pause => tokio::time::sleep(Duration::from_millis(30)).await,
                        Close => return,
                    }
                }
                while read_head(&mut s).await.is_some() {
                    let body = format!("conn={n}");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if s.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (plain(addr), count)
}

async fn read_head(s: &mut TcpStream) -> Option<()> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    Some(())
}

fn plain(addr: SocketAddr) -> Target {
    Target::new(addr, "localhost", Transport::Plain)
}

fn get() -> Request<toolkit_http_proxy::ProxyBody> {
    Request::get("/").body(empty_body()).unwrap()
}

fn head() -> Request<toolkit_http_proxy::ProxyBody> {
    Request::head("/").body(empty_body()).unwrap()
}

async fn text(resp: Response<ResponseBody>) -> String {
    String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

/// The body of a follow-up request: `conn=1` means the first connection
/// was reused, `conn=2` that a new one was opened.
async fn next_conn(c: &ProxyClient, t: &Target) -> String {
    // Let the engine settle the previous exchange (and any late bytes land).
    tokio::time::sleep(Duration::from_millis(50)).await;
    text(c.send(t, get()).await.unwrap()).await
}

// ---------------------------------------------------------------------------
// Response heads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lenient_response_heads_are_normalized() {
    // Whitespace before the colon (also on the framing header) and an
    // obs-fold: accepted and normalized, as RFC 9112 asks of a proxy.
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length : 4\r\nX-Spaced : 1\r\nX-Folded: a\r\n b\r\n\r\ngood"),
    ];
    let (t, conns) = scripted(&[SCRIPT]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    let h = resp.headers();
    assert_eq!(h["x-spaced"], "1");
    let folded = h["x-folded"].to_str().unwrap();
    assert!(
        folded.starts_with('a') && folded.ends_with('b'),
        "{folded:?}"
    );
    assert!(!folded.contains(['\r', '\n']), "{folded:?}");
    assert_eq!(h.get_all("content-length").iter().count(), 1);
    assert_eq!(h["content-length"], "4");
    assert_eq!(text(resp).await, "good");
    // The length framed the body exactly: the connection stays in sync.
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn head_split_across_reads_is_reassembled() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r"),
        Pause,
        Write(b"\nContent-Length: 2\r\nX-A: 1\r"),
        Pause,
        Write(b"\n\r"),
        Pause,
        Write(b"\nok"),
    ];
    let (t, _) = scripted(&[SCRIPT]).await;
    let resp = support::client().send(&t, get()).await.unwrap();
    assert_eq!(resp.headers()["x-a"], "1");
    assert_eq!(text(resp).await, "ok");
}

#[tokio::test]
async fn custom_reason_phrase_is_kept() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 Just Fine\r\nContent-Length: 0\r\n\r\n"),
    ];
    let (t, _) = scripted(&[SCRIPT]).await;
    let resp = support::client().send(&t, get()).await.unwrap();
    let reason = resp.extensions().get::<hyper::ext::ReasonPhrase>().unwrap();
    assert_eq!(reason.as_bytes(), b"Just Fine");
}

#[tokio::test]
async fn invalid_content_length_is_rejected_and_not_pooled() {
    static CASES: [&[Step]; 5] = [
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n"),
        ],
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\n\r\n"),
        ],
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: 1.5\r\n\r\n"),
        ],
        &[Read, Write(b"HTTP/1.1 200 OK\r\nContent-Length: \r\n\r\n")],
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: 123abc\r\n\r\n"),
        ],
    ];
    for (i, script) in CASES.iter().enumerate() {
        let scripts = [*script, &[]];
        let (t, conns) = scripted(&scripts).await;
        let c = support::client();
        let err = c
            .send(&t, get())
            .await
            .expect_err("invalid length accepted");
        assert_eq!(err.kind(), ErrorKind::Protocol, "case {i}");
        assert_eq!(next_conn(&c, &t).await, "conn=2", "case {i}");
        assert_eq!(conns.load(Ordering::SeqCst), 2, "case {i}");
    }
}

#[tokio::test]
async fn identical_duplicate_content_lengths_are_accepted() {
    static CASES: [&[Step]; 2] = [
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\ngood"),
        ],
        &[
            Read,
            Write(b"HTTP/1.1 200 OK\r\nContent-Length: 4, 4\r\n\r\ngood"),
        ],
    ];
    for script in CASES {
        let scripts = [script];
        let (t, conns) = scripted(&scripts).await;
        let c = support::client();
        assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "good");
        assert_eq!(next_conn(&c, &t).await, "conn=1");
        assert_eq!(conns.load(Ordering::SeqCst), 1);
    }
}

// ---------------------------------------------------------------------------
// Transfer-Encoding and chunked bodies
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chunked_is_decided_by_the_last_transfer_coding() {
    // `gzip` then `chunked` on two lines: chunked (the coding is passed on
    // undecoded; the engine never decompresses).
    static CHUNKED: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[CHUNKED]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "abcd");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    // `chunked` then `identity`: not chunked, so the body runs to the close
    // and the connection is not reused.
    let to_close: &'static [Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: identity\r\n\r\nraw bytes"),
        Close,
    ];
    let (t, conns) = scripted(&[to_close, &[]]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "raw bytes");
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn chunk_extensions_are_ignored() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;name=value;flag\r\nabcd\r\n0;last\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[SCRIPT]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "abcd");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn overflowing_chunk_size_fails_and_is_not_pooled() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\nabcd\r\n0\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    assert!(resp.into_body().collect().await.is_err());
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn http1_trailers_reach_the_caller_sanitized() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum, Content-Length\r\n\r\n\
              4\r\nabcd\r\n0\r\nX-Checksum: c0ffee\r\nContent-Length: 99\r\nHost: evil.example\r\n\r\n",
        ),
    ];
    let (t, conns) = scripted(&[SCRIPT]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    // The declaration survives, minus what trailers may not carry.
    assert_eq!(resp.headers()["trailer"], "X-Checksum");
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().cloned().unwrap_or_else(HeaderMap::new);
    assert_eq!(collected.to_bytes(), "abcd");
    assert_eq!(trailers["x-checksum"], "c0ffee");
    assert!(trailers.get("content-length").is_none());
    assert!(trailers.get("host").is_none());
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn trailers_lose_what_the_head_connection_nominated() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(
            b"HTTP/1.1 200 OK\r\nConnection: X-Hop\r\nTransfer-Encoding: chunked\r\nTrailer: X-Hop, X-Checksum\r\n\r\n\
              2\r\nok\r\n0\r\nX-Hop: 1\r\nX-Checksum: c0ffee\r\nProxy-Authenticate: Basic\r\n\r\n",
        ),
    ];
    let (t, _) = scripted(&[SCRIPT]).await;
    let resp = support::client().send(&t, get()).await.unwrap();
    assert_eq!(resp.headers()["trailer"], "X-Checksum");
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().cloned().unwrap_or_else(HeaderMap::new);
    assert_eq!(trailers["x-checksum"], "c0ffee");
    assert!(trailers.get("x-hop").is_none());
    assert!(trailers.get("proxy-authenticate").is_none());
}

#[tokio::test]
async fn oversized_trailers_fail_the_body() {
    let mut bytes =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n".to_vec();
    for i in 0..64 {
        bytes.extend_from_slice(format!("x-t{i}: {}\r\n", "v".repeat(1024)).as_bytes());
    }
    bytes.extend_from_slice(b"\r\n");
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let script: &'static [Step] = Box::leak(Box::new([Read, Write(bytes)]));
    let scripts = [script, &[]];
    let (t, _) = scripted(&scripts).await;
    let resp = support::client().send(&t, get()).await.unwrap();
    assert!(resp.into_body().collect().await.is_err());
}

// ---------------------------------------------------------------------------
// Keep-alive decisions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_delimited_body_is_read_to_the_close_and_not_pooled() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\n\r\nuntil the close"),
        Close,
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    assert_eq!(
        text(c.send(&t, get()).await.unwrap()).await,
        "until the close"
    );
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn http10_responses_are_pooled_only_with_keep_alive() {
    static PLAIN: &[Step] = &[
        Read,
        Write(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    ];
    let (t, conns) = scripted(&[PLAIN, &[]]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "ok");
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);

    let keep_alive: &'static [Step] = &[
        Read,
        Write(b"HTTP/1.0 200 OK\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\nok"),
    ];
    let (t, conns) = scripted(&[keep_alive]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "ok");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn connection_close_is_honoured_and_not_relayed() {
    // The upstream says `close` but keeps the socket open: the engine must
    // still not reuse it.
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nConnection: close, X-Hop\r\nX-Hop: 1\r\nContent-Length: 2\r\n\r\nok"),
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    assert!(resp.headers().get("connection").is_none());
    assert!(resp.headers().get("x-hop").is_none());
    assert_eq!(text(resp).await, "ok");
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bytes_beyond_the_content_length_retire_the_connection() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokX"),
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "ok");
    // The extra byte is never read as the start of the next response.
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unsolicited_bytes_on_an_idle_connection_retire_it() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
        Pause,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nevil"),
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    assert_eq!(text(c.send(&t, get()).await.unwrap()).await, "ok");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// Interim (1xx) responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn interim_responses_are_skipped_and_the_connection_kept() {
    static TOGETHER: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    ];
    static APART: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 100 Continue\r\n\r\n"),
        Pause,
        Write(b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\n\r\n"),
        Pause,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    ];
    for script in [TOGETHER, APART] {
        let scripts = [script];
        let (t, conns) = scripted(&scripts).await;
        let c = support::client();
        let resp = c.send(&t, get()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("link").is_none(),
            "interim headers leaked"
        );
        assert_eq!(text(resp).await, "ok");
        assert_eq!(next_conn(&c, &t).await, "conn=1");
        assert_eq!(conns.load(Ordering::SeqCst), 1);
    }

    // Several interim responses and a bodiless final one, in one write.
    let no_content: &'static [Step] = &[
        Read,
        Write(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[no_content]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(text(resp).await, "");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Bodiless responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bodiless_responses_keep_the_connection() {
    // HEAD: the length describes the representation, no body follows.
    static HEAD: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[HEAD]).await;
    let c = support::client();
    let resp = c.send(&t, head()).await.unwrap();
    assert_eq!(resp.headers()["content-length"], "1000");
    assert!(resp.body().is_end_stream());
    assert_eq!(text(resp).await, "");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    // 204 with a length: no body, and the length is not relayed.
    let no_content: &'static [Step] = &[
        Read,
        Write(b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[no_content]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    assert!(resp.headers().get("content-length").is_none());
    assert_eq!(text(resp).await, "");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    // 304 with a length: no body, the length is the representation's.
    let not_modified: &'static [Step] = &[
        Read,
        Write(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 1000\r\n\r\n"),
    ];
    let (t, conns) = scripted(&[not_modified]).await;
    let c = support::client();
    let resp = c.send(&t, get()).await.unwrap();
    assert_eq!(resp.headers()["content-length"], "1000");
    assert_eq!(text(resp).await, "");
    assert_eq!(next_conn(&c, &t).await, "conn=1");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Upstream closing early
// ---------------------------------------------------------------------------

#[tokio::test]
async fn truncated_body_delivers_its_data_then_fails() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"),
        Close,
    ];
    let (t, conns) = scripted(&[SCRIPT, &[]]).await;
    let c = support::client();
    let mut body = c.send(&t, get()).await.unwrap().into_body();
    let mut received = Vec::new();
    let err = loop {
        match body.frame().await.expect("an error before the end") {
            Ok(frame) => received.extend_from_slice(&frame.into_data().unwrap()),
            Err(e) => break e,
        }
    };
    assert_eq!(received, b"abc");
    assert_eq!(err.kind(), ErrorKind::Read);
    assert_eq!(next_conn(&c, &t).await, "conn=2");
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn close_right_after_the_head_fails_the_body() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n"),
        Close,
    ];
    let (t, _) = scripted(&[SCRIPT, &[]]).await;
    let resp = support::client().send(&t, get()).await.unwrap();
    let err = resp.into_body().collect().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Read);
}

#[tokio::test]
async fn partial_head_then_close_fails_without_a_resend() {
    // A fresh connection: nothing proves the request was not processed.
    static SCRIPT: &[Step] = &[Read, Write(b"HTTP/1.1 200 OK\r\nContent-Le"), Close];
    let (t, conns) = scripted(&[SCRIPT]).await;
    let err = support::client().send(&t, get()).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectionClosed);
    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Upgrades
// ---------------------------------------------------------------------------

fn ws_request() -> Request<toolkit_http_proxy::ProxyBody> {
    Request::get("/ws")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(empty_body())
        .unwrap()
}

#[tokio::test]
async fn tunnel_bytes_sent_with_the_101_come_first() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\nfirst bytes"),
        Pause,
    ];
    let (t, _) = scripted(&[SCRIPT]).await;
    let outcome = support::client().upgrade(&t, ws_request()).await.unwrap();
    let toolkit_http_proxy::UpgradeOutcome::Switched { io, .. } = outcome else {
        panic!("expected a switch");
    };
    let mut io = hyper_util::rt::TokioIo::new(io);
    let mut buf = [0u8; 11];
    tokio::time::timeout(Duration::from_secs(2), io.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, b"first bytes");
}

#[tokio::test]
async fn http10_101_is_not_a_switch() {
    static SCRIPT: &[Step] = &[
        Read,
        Write(b"HTTP/1.0 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"),
        Pause,
    ];
    let (t, _) = scripted(&[SCRIPT]).await;
    let err = support::client()
        .upgrade(&t, ws_request())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Protocol);
}

// ---------------------------------------------------------------------------
// Header case, end to end through a hyper server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn header_case_survives_a_hyper_server_and_the_engine() {
    let (response, upstream_head) = through_a_case_keeping_server(true).await;
    assert!(response.contains("X-CamelCase: 1"), "{response}");
    assert!(upstream_head.contains("X-MiXeD-Case: v"), "{upstream_head}");

    // Off by default: a request's case map (recorded by the caller's server)
    // is still used, at no cost to the engine; the response's case is not
    // recorded, so it comes back in hyper's lower case.
    let (response, upstream_head) = through_a_case_keeping_server(false).await;
    assert!(response.contains("x-camelcase: 1"), "{response}");
    assert!(upstream_head.contains("X-MiXeD-Case: v"), "{upstream_head}");
}

/// A raw client → a hyper server that keeps header case → the engine (with
/// `preserve_header_case` as given) → a raw upstream. Returns the response
/// head the client got and the request head the upstream got.
async fn through_a_case_keeping_server(preserve: bool) -> (String, String) {
    // Upstream: records the request bytes, answers with a mixed-case header.
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = plain(upstream.local_addr().unwrap());
    let seen = tokio::spawn(async move {
        let (mut s, _) = upstream.accept().await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        s.write_all(b"HTTP/1.1 200 OK\r\nX-CamelCase: 1\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        String::from_utf8(head).unwrap()
    });

    // A hyper server that keeps header case and forwards through the engine.
    let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let front_addr = front.local_addr().unwrap();
    let mut cfg = support::test_config();
    cfg.preserve_header_case = preserve;
    let c = ProxyClient::new(cfg).unwrap();
    tokio::spawn(async move {
        let (s, _) = front.accept().await.unwrap();
        let svc = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
            let (c, target) = (c.clone(), target.clone());
            async move {
                let (mut parts, body) = req.into_parts();
                parts.uri = "/".parse().unwrap();
                let req = Request::from_parts(parts, body.map_err(Into::into).boxed_unsync());
                c.send(&target, req).await
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .preserve_header_case(true)
            .serve_connection(hyper_util::rt::TokioIo::new(s), svc)
            .await;
    });

    let mut client = TcpStream::connect(front_addr).await.unwrap();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nX-MiXeD-Case: v\r\n\r\n")
        .await
        .unwrap();
    // The whole response head, however it is split across reads.
    let mut response = Vec::new();
    let mut buf = [0u8; 512];
    while !response.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0, "connection closed before the response head");
        response.extend_from_slice(&buf[..n]);
    }
    let response = String::from_utf8_lossy(&response).into_owned();
    (response, seen.await.unwrap())
}

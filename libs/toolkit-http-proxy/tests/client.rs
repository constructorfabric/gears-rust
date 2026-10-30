//! Functional tests of the proxy engine against real local upstreams.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    clippy::use_debug
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use http::{Request, StatusCode, Version};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Body as _, Frame, Incoming};
use support::{Wire, body_string, client, echo, test_config, text, upstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use toolkit_http_proxy::{
    CachedProtocol, ConnectionInfo, ErrorKind, Observer, ProtocolPolicy, ProxyClient, RootStore,
    Target, Transport, UpgradeOutcome, empty_body, full_body, stream_body,
};

fn get(path: &str) -> Request<toolkit_http_proxy::ProxyBody> {
    Request::get(path).body(empty_body()).unwrap()
}

#[tokio::test]
async fn h1_round_trip_and_keep_alive_reuse() {
    let up = echo(Wire::Plain).await;
    let c = client();
    let t = up.target(Wire::Plain);
    for i in 0..5 {
        let req = Request::post(format!("/items/{i}?q=1"))
            .body(full_body(Bytes::from_static(b"hello")))
            .unwrap();
        let resp = c.send(&t, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert_eq!(
            body,
            format!(
                "POST /items/{i}?q=1 HTTP/1.1 host=localhost:{} len=5",
                up.addr.port()
            )
        );
    }
    assert_eq!(up.accepted(), 1, "keep-alive connection is reused");
}

#[tokio::test]
async fn h1_streamed_exchanges_keep_the_connection() {
    // A streamed upload and a chunked response far larger than the read
    // buffer: the exchange that reads them also drives the connection, which
    // must end each one ready for the next.
    let up = upstream(Wire::Plain, |req: Request<Incoming>| async move {
        let uploaded = req.into_body().collect().await.unwrap().to_bytes().len();
        assert_eq!(uploaded, 8 * 32 * 1024);
        let frames = stream::iter((0..64).map(|_| {
            Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from(vec![b'r'; 16 * 1024])))
        }));
        http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
    })
    .await;
    let c = client();
    let t = up.target(Wire::Plain);
    for _ in 0..5 {
        let chunks = stream::iter(
            (0..8).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'u'; 32 * 1024]))),
        );
        let req = Request::post("/").body(stream_body(chunks)).unwrap();
        let resp = c.send(&t, req).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 1024 * 1024);
    }
    assert_eq!(up.accepted(), 1, "keep-alive connection is reused");
}

#[tokio::test]
async fn h1_concurrent_requests_open_parallel_connections() {
    let up = echo(Wire::Plain).await;
    let c = client();
    let t = up.target(Wire::Plain);
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let (c, t) = (c.clone(), t.clone());
            tokio::spawn(async move { body_string(c.send(&t, get("/")).await.unwrap()).await })
        })
        .collect();
    for task in tasks {
        assert!(task.await.unwrap().starts_with("GET / HTTP/1.1"));
    }
    assert!(up.accepted() <= 16);
}

#[tokio::test]
async fn h2_over_tls_multiplexes_one_connection() {
    let up = echo(Wire::TlsH2).await;
    let mut cfg = test_config();
    cfg.pool.h2_streams_per_connection = 64;
    let c = ProxyClient::new(cfg).unwrap();
    let t = up.target(Wire::TlsH2);
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let (c, t) = (c.clone(), t.clone());
            tokio::spawn(
                async move { body_string(c.send(&t, get("/v1/models")).await.unwrap()).await },
            )
        })
        .collect();
    for task in tasks {
        let body = task.await.unwrap();
        assert!(
            body.starts_with(&format!(
                "GET https://localhost:{}/v1/models HTTP/2.0",
                up.addr.port()
            )),
            "{body}"
        );
    }
    assert_eq!(up.accepted(), 1, "one multiplexed HTTP/2 connection");
    assert_eq!(c.cached_protocol(&t), Some(CachedProtocol::Http2));
}

#[tokio::test]
async fn h2_load_spreads_over_a_bounded_set_of_connections() {
    let up = upstream(Wire::TlsH2, |_req: Request<Incoming>| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        text("slow")
    })
    .await;
    let mut cfg = test_config();
    cfg.pool.h2_streams_per_connection = 4;
    cfg.pool.h2_max_connections_per_host = 3;
    let c = ProxyClient::new(cfg).unwrap();
    let t = up.target(Wire::TlsH2);
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let (c, t) = (c.clone(), t.clone());
            tokio::spawn(async move { body_string(c.send(&t, get("/")).await.unwrap()).await })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap(), "slow");
    }
    assert_eq!(up.accepted(), 3, "grows to the cap, then shares");
}

#[tokio::test]
async fn alpn_negotiates_and_caches_per_host() {
    let both = echo(Wire::TlsBoth).await;
    let h1 = echo(Wire::TlsH1).await;
    let c = client();

    let tb = both.target(Wire::TlsBoth);
    assert!(
        body_string(c.send(&tb, get("/")).await.unwrap())
            .await
            .contains("HTTP/2.0")
    );
    assert_eq!(c.cached_protocol(&tb), Some(CachedProtocol::Http2));

    let t1 = h1.target(Wire::TlsH1);
    assert!(
        body_string(c.send(&t1, get("/")).await.unwrap())
            .await
            .contains("HTTP/1.1")
    );
    assert_eq!(c.cached_protocol(&t1), Some(CachedProtocol::Http1Only));
    // Cached HTTP/1.1 keeps using the pooled connection.
    assert!(
        body_string(c.send(&t1, get("/")).await.unwrap())
            .await
            .contains("HTTP/1.1")
    );
    assert_eq!(h1.accepted(), 1);
}

#[tokio::test]
async fn disabled_protocol_cache_still_pools() {
    let up = echo(Wire::TlsH1).await;
    let mut cfg = test_config();
    cfg.protocol_cache_ttl = Duration::ZERO;
    let c = ProxyClient::new(cfg).unwrap();
    let t = up.target(Wire::TlsH1);
    for _ in 0..3 {
        assert!(
            body_string(c.send(&t, get("/")).await.unwrap())
                .await
                .contains("HTTP/1.1")
        );
    }
    assert_eq!(c.cached_protocol(&t), None);
    assert_eq!(up.accepted(), 1);
}

#[tokio::test]
async fn h2_prior_knowledge_over_cleartext() {
    let up = echo(Wire::PlainH2).await;
    let c = client();
    let body = body_string(c.send(&up.target(Wire::PlainH2), get("/x")).await.unwrap()).await;
    assert!(body.contains("HTTP/2.0"), "{body}");
}

#[tokio::test]
async fn streaming_request_bodies_are_forwarded() {
    for wire in [Wire::Plain, Wire::TlsH2] {
        let up = echo(wire).await;
        let c = client();
        let chunks =
            stream::iter((0..10).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 1000]))));
        let req = Request::post("/upload").body(stream_body(chunks)).unwrap();
        let body = body_string(c.send(&up.target(wire), req).await.unwrap()).await;
        assert!(body.ends_with("len=10000"), "{wire:?}: {body}");
    }
}

#[tokio::test]
async fn h2_request_trailers_lose_hop_fields() {
    // The upstream answers with the names of the trailers it received.
    let up = upstream(Wire::TlsH2, |req: Request<Incoming>| async move {
        let collected = req.into_body().collect().await.unwrap();
        let mut names: Vec<String> = collected
            .trailers()
            .map(|t| t.keys().map(|k| k.as_str().to_owned()).collect())
            .unwrap_or_default();
        names.sort_unstable();
        text(names.join(","))
    })
    .await;
    let mut trailers = http::HeaderMap::new();
    for (name, value) in [
        ("grpc-status", "0"),
        ("x-hop", "1"),
        ("proxy-authorization", "Basic x"),
        ("content-length", "7"),
    ] {
        trailers.insert(name, http::HeaderValue::from_static(value));
    }
    let frames = stream::iter([
        Frame::data(Bytes::from_static(b"payload")),
        Frame::trailers(trailers),
    ])
    .map(Ok::<_, Box<dyn std::error::Error + Send + Sync>>);
    let req = Request::post("/")
        .header("connection", "x-hop")
        .body(BodyExt::boxed_unsync(StreamBody::new(frames)))
        .unwrap();
    let names = body_string(client().send(&up.target(Wire::TlsH2), req).await.unwrap()).await;
    assert_eq!(names, "grpc-status");
}

#[tokio::test]
async fn h2_requests_state_a_known_body_length() {
    // The upstream answers with the `content-length` it received.
    let up = upstream(Wire::TlsH2, |req: Request<Incoming>| async move {
        let len = req
            .headers()
            .get("content-length")
            .map_or("none".to_owned(), |v| v.to_str().unwrap().to_owned());
        let _ = req.into_body().collect().await;
        text(len)
    })
    .await;
    let c = client();
    let t = up.target(Wire::TlsH2);
    let unknown = || stream_body(stream::iter([Ok::<_, std::io::Error>(Bytes::from("abc"))]));
    for (name, req, expected) in [
        (
            "buffered POST",
            Request::post("/").body(full_body(Bytes::from_static(b"hello"))),
            "5",
        ),
        (
            "caller's length is not trusted",
            Request::post("/")
                .header("content-length", "99")
                .body(full_body(Bytes::from_static(b"hello"))),
            "5",
        ),
        ("empty POST", Request::post("/").body(empty_body()), "0"),
        ("empty GET", Request::get("/").body(empty_body()), "none"),
        ("streamed POST", Request::post("/").body(unknown()), "none"),
    ] {
        let resp = c.send(&t, req.unwrap()).await.unwrap();
        assert_eq!(body_string(resp).await, expected, "{name}");
    }
}

#[tokio::test]
async fn large_response_bodies_stream() {
    let up = upstream(Wire::TlsBoth, |_req: Request<Incoming>| async {
        text(vec![b'z'; 4 * 1024 * 1024])
    })
    .await;
    let c = client();
    let resp = c.send(&up.target(Wire::TlsBoth), get("/")).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.len(), 4 * 1024 * 1024);
}

#[tokio::test]
async fn response_head_timeout() {
    let up = upstream(Wire::Plain, |_req: Request<Incoming>| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        text("late")
    })
    .await;
    let mut cfg = test_config();
    cfg.read_timeout = Some(Duration::from_millis(200));
    let c = ProxyClient::new(cfg).unwrap();
    let err = c.send(&up.target(Wire::Plain), get("/")).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadTimeout);
}

#[tokio::test]
async fn body_idle_timeout() {
    let up = upstream(Wire::Plain, |_req: Request<Incoming>| async {
        let frames = stream::unfold(0u8, |n| async move {
            if n == 0 {
                Some((
                    Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from_static(b"first"))),
                    1,
                ))
            } else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                None
            }
        });
        http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
    })
    .await;
    let mut cfg = test_config();
    cfg.read_timeout = Some(Duration::from_millis(300));
    let c = ProxyClient::new(cfg).unwrap();
    let resp = c.send(&up.target(Wire::Plain), get("/")).await.unwrap();
    let mut body = resp.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(&first[..], b"first");
    let err = body.frame().await.unwrap().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadTimeout);
}

#[tokio::test]
async fn body_idle_timeout_can_be_replaced_per_response() {
    let up = upstream(Wire::Plain, |_req: Request<Incoming>| async {
        let frames = stream::unfold(0u8, |n| async move {
            if n < 2 {
                // Each frame arrives after more than the client's read timeout.
                tokio::time::sleep(Duration::from_millis(500)).await;
                Some((
                    Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from_static(b"tick"))),
                    n + 1,
                ))
            } else {
                None
            }
        });
        http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
    })
    .await;
    let mut cfg = test_config();
    cfg.read_timeout = Some(Duration::from_millis(200));
    let c = ProxyClient::new(cfg).unwrap();
    for limit in [Some(Duration::from_secs(2)), None] {
        let resp = c.send(&up.target(Wire::Plain), get("/")).await.unwrap();
        let mut body = resp.into_body();
        body.set_idle_timeout(limit);
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"ticktick", "limit {limit:?}");
    }
}

/// An upstream answering with `n` small chunks sent back to back.
async fn chunked_burst(wire: Wire, n: usize) -> support::Upstream {
    upstream(wire, move |_req: Request<Incoming>| async move {
        let frames = stream::iter((0..n).map(|i| {
            Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from(format!(
                "data: event {i:05}\n\n"
            ))))
        }));
        http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
    })
    .await
}

/// Far fewer frames than the 5000 chunks of a burst, which come one per
/// frame without merging.
const FEW_FRAMES: usize = 500;

fn burst_bytes(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("data: event {i:05}\n\n").into_bytes())
        .collect()
}

#[tokio::test]
async fn chunks_that_already_arrived_are_merged() {
    const EVENTS: usize = 5000;
    for wire in [Wire::Plain, Wire::TlsH2] {
        let up = chunked_burst(wire, EVENTS).await;
        let c = client();
        let resp = c.send(&up.target(wire), get("/")).await.unwrap();
        let mut body = resp.into_body();
        let mut received = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec();
        // Let the rest of the burst arrive, then read it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut frames = 0;
        while let Some(frame) = body.frame().await {
            let data = frame.unwrap().into_data().unwrap();
            // At most 128 upstream chunks per frame.
            assert!(
                data.len() <= 128 * burst_bytes(1).len(),
                "{wire:?}: {} bytes in one frame",
                data.len()
            );
            received.extend_from_slice(&data);
            frames += 1;
        }
        assert_eq!(received, burst_bytes(EVENTS), "{wire:?}");
        // Far fewer frames than chunks (one per chunk without merging).
        assert!(frames <= FEW_FRAMES, "{wire:?}: {frames} frames");
        // The merged body still reached its clean end: the connection is reused.
        let _ = body_string(c.send(&up.target(wire), get("/")).await.unwrap()).await;
        assert_eq!(up.accepted(), 1, "{wire:?}");
    }
}

#[tokio::test]
async fn merge_limit_is_configurable() {
    const EVENTS: usize = 5000;
    for wire in [Wire::Plain, Wire::TlsH2] {
        let up = chunked_burst(wire, EVENTS).await;
        let event = burst_bytes(1).len();
        for (limit, max_frame) in [(0, event), (1024, 1024)] {
            let mut cfg = test_config();
            cfg.response_merge_limit = limit;
            let c = ProxyClient::new(cfg).unwrap();
            let resp = c.send(&up.target(wire), get("/")).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut body = resp.into_body();
            let (mut received, mut frames) = (Vec::new(), 0);
            while let Some(frame) = body.frame().await {
                let data = frame.unwrap().into_data().unwrap();
                assert!(
                    data.len() <= max_frame,
                    "{wire:?} limit {limit}: {} bytes in one frame",
                    data.len()
                );
                received.extend_from_slice(&data);
                frames += 1;
            }
            assert_eq!(received, burst_bytes(EVENTS), "{wire:?} limit {limit}");
            // One frame per chunk without merging; with it, at least size /
            // limit frames and far fewer than one per chunk.
            match received.len().checked_div(limit) {
                None => assert!(frames >= EVENTS, "{wire:?} unmerged: {frames} frames"),
                Some(min) => assert!(
                    (min..=FEW_FRAMES).contains(&frames),
                    "{wire:?} limit {limit}: {frames} frames"
                ),
            }
        }
    }
}

#[tokio::test]
async fn merging_never_waits_for_more_data() {
    for wire in [Wire::Plain, Wire::TlsH2] {
        let next = Arc::new(tokio::sync::Notify::new());
        let gate = next.clone();
        let up = upstream(wire, move |_req: Request<Incoming>| {
            let gate = gate.clone();
            async move {
                let frames = stream::unfold(0u8, move |n| {
                    let gate = gate.clone();
                    async move {
                        match n {
                            0 => Some((Bytes::from_static(b"first"), 1)),
                            1 => {
                                gate.notified().await;
                                Some((Bytes::from_static(b"second"), 2))
                            }
                            _ => None,
                        }
                    }
                })
                .map(|data| Ok::<_, std::convert::Infallible>(Frame::data(data)));
                http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
            }
        })
        .await;
        let c = client();
        let resp = c.send(&up.target(wire), get("/")).await.unwrap();
        let mut body = resp.into_body();
        // The upstream sends "second" only after this frame arrived.
        let first = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("the first chunk must not wait for the second")
            .unwrap()
            .unwrap();
        assert_eq!(&first.into_data().unwrap()[..], b"first", "{wire:?}");
        next.notify_one();
        let second = body.collect().await.unwrap().to_bytes();
        assert_eq!(&second[..], b"second", "{wire:?}");
    }
}

#[tokio::test]
async fn merged_data_is_followed_by_the_trailers() {
    let up = upstream(Wire::TlsH2, |_req: Request<Incoming>| async {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
        let frames = stream::iter(
            (0..100)
                .map(|_| Frame::data(Bytes::from_static(b"0123456789")))
                .chain(std::iter::once(Frame::trailers(trailers)))
                .map(Ok::<_, std::convert::Infallible>),
        );
        http::Response::new(BodyExt::boxed(StreamBody::new(frames)))
    })
    .await;
    let c = client();
    let resp = c.send(&up.target(Wire::TlsH2), get("/")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut body = resp.into_body();
    let (mut data, mut trailers) = (0, Vec::new());
    loop {
        let end = body.is_end_stream();
        let Some(frame) = body.frame().await else {
            break;
        };
        // Whatever is still to come (merged data, stashed trailers) keeps
        // the body from reporting its end.
        assert!(!end, "is_end_stream before a frame");
        match frame.unwrap().into_data() {
            Ok(bytes) => {
                assert!(trailers.is_empty(), "data after the trailers");
                data += bytes.len();
            }
            Err(frame) => trailers.push(frame.into_trailers().unwrap()),
        }
    }
    assert_eq!(data, 1000);
    assert_eq!(trailers.len(), 1);
    assert_eq!(
        trailers[0].get("grpc-status"),
        Some(&http::HeaderValue::from_static("0"))
    );
    assert!(body.is_end_stream());
}

#[tokio::test]
async fn data_before_a_broken_body_is_delivered_first() {
    // 100 chunks, then the connection closes without the last chunk.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = tcp.read(&mut buf).await.unwrap();
            head.extend_from_slice(&buf[..n]);
        }
        let mut resp = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..100 {
            resp.extend_from_slice(b"a\r\n0123456789\r\n");
        }
        tcp.write_all(&resp).await.unwrap();
    });
    let c = client();
    let resp = c
        .send(&Target::new(addr, "localhost", Transport::Plain), get("/"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut body = resp.into_body();
    let mut received = 0;
    let err = loop {
        match body.frame().await.expect("an error before the end") {
            Ok(frame) => received += frame.into_data().unwrap().len(),
            Err(err) => break err,
        }
    };
    assert_eq!(received, 1000);
    assert_eq!(err.kind(), ErrorKind::Read);
    assert!(body.frame().await.is_none());
}

#[tokio::test]
async fn connect_refused_is_classified() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let err = client()
        .send(&Target::new(addr, "localhost", Transport::Plain), get("/"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectRefused, "{err}");
}

#[tokio::test]
async fn untrusted_certificate_is_rejected() {
    let up = echo(Wire::TlsBoth).await;
    let mut cfg = test_config();
    cfg.tls.roots = RootStore::WebPki;
    let c = ProxyClient::new(cfg).unwrap();
    let err = c
        .send(&up.target(Wire::TlsBoth), get("/"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidCertificate, "{err}");
}

#[tokio::test]
async fn wrong_server_name_is_rejected() {
    let up = echo(Wire::TlsBoth).await;
    let t = Target::new(up.addr, "not-localhost.example", Transport::Tls);
    let err = client().send(&t, get("/")).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidCertificate, "{err}");
}

#[cfg(not(feature = "fips"))]
#[tokio::test]
async fn danger_accept_any_skips_verification_outside_fips() {
    let up = echo(Wire::TlsBoth).await;
    let mut cfg = test_config();
    cfg.tls.roots = RootStore::WebPki;
    cfg.tls.verification = toolkit_http_proxy::CertVerification::DangerAcceptAny;
    let c = ProxyClient::new(cfg).unwrap();
    assert!(c.send(&up.target(Wire::TlsBoth), get("/")).await.is_ok());
}

#[cfg(feature = "fips")]
#[tokio::test]
async fn danger_accept_any_is_refused_under_fips() {
    let mut cfg = test_config();
    cfg.tls.verification = toolkit_http_proxy::CertVerification::DangerAcceptAny;
    let err = ProxyClient::new(cfg).err().expect("refused");
    assert_eq!(err.kind(), ErrorKind::Configuration);
}

#[tokio::test]
async fn half_read_body_closes_the_connection() {
    let up = upstream(Wire::Plain, |req: Request<Incoming>| async move {
        if req.uri().path() == "/big" {
            text(vec![b'b'; 2 * 1024 * 1024])
        } else {
            text("small")
        }
    })
    .await;
    let c = client();
    let t = up.target(Wire::Plain);
    let resp = c.send(&t, get("/big")).await.unwrap();
    let mut body = resp.into_body();
    let _ = body.frame().await;
    drop(body);
    assert_eq!(
        body_string(c.send(&t, get("/s")).await.unwrap()).await,
        "small"
    );
    assert_eq!(up.accepted(), 2, "a half-read connection is never reused");
}

#[tokio::test]
async fn pool_is_keyed_by_server_name() {
    let up = echo(Wire::TlsH1).await;
    let c = client();
    let a = Target::new(up.addr, "localhost", Transport::Tls);
    let b = Target::new(up.addr, "127.0.0.1", Transport::Tls);
    c.send(&a, get("/"))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    c.send(&b, get("/"))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    c.send(&a, get("/"))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    assert_eq!(
        up.accepted(),
        2,
        "same address, different SNI: separate connections"
    );
}

#[derive(Default)]
struct Recorder {
    new: AtomicUsize,
    reused: AtomicUsize,
}

impl Observer for Recorder {
    fn on_connection(&self, info: &ConnectionInfo<'_>) {
        if info.reused {
            self.reused.fetch_add(1, Ordering::SeqCst);
        } else {
            assert!(info.connect_time.is_some());
            self.new.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn observer_sees_new_and_reused_connections() {
    let up = echo(Wire::Plain).await;
    let rec = Arc::new(Recorder::default());
    let c = ProxyClient::with_observer(test_config(), rec.clone()).unwrap();
    let t = up.target(Wire::Plain);
    for _ in 0..3 {
        body_string(c.send(&t, get("/")).await.unwrap()).await;
    }
    assert_eq!(rec.new.load(Ordering::SeqCst), 1);
    assert_eq!(rec.reused.load(Ordering::SeqCst), 2);
}

/// Upstream that switches to a byte-echo tunnel on a websocket upgrade.
async fn ws_echo(wire: Wire) -> support::Upstream {
    upstream(wire, |mut req: Request<Incoming>| async move {
        if !toolkit_http_proxy::sanitize::is_websocket_upgrade(req.headers()) {
            return text("no upgrade");
        }
        let on_upgrade = hyper::upgrade::on(&mut req);
        tokio::spawn(async move {
            if let Ok(upgraded) = on_upgrade.await {
                let mut io = hyper_util::rt::TokioIo::new(upgraded);
                let mut buf = [0u8; 64];
                while let Ok(n) = io.read(&mut buf).await {
                    if n == 0 || io.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        });
        let mut resp = text("");
        *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        resp.headers_mut()
            .insert("upgrade", "websocket".parse().unwrap());
        resp.headers_mut()
            .insert("connection", "upgrade".parse().unwrap());
        resp
    })
    .await
}

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
async fn websocket_upgrade_tunnels_after_101() {
    for wire in [Wire::Plain, Wire::TlsBoth] {
        let up = ws_echo(wire).await;
        let outcome = client()
            .upgrade(&up.target(wire), ws_request())
            .await
            .unwrap();
        let UpgradeOutcome::Switched { response, io } = outcome else {
            panic!("{wire:?}: expected a switch");
        };
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(response.headers()["upgrade"], "websocket");
        let mut io = hyper_util::rt::TokioIo::new(io);
        io.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        io.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}

#[tokio::test]
async fn websocket_rejection_is_a_plain_response() {
    let up = echo(Wire::Plain).await;
    let outcome = client()
        .upgrade(&up.target(Wire::Plain), ws_request())
        .await
        .unwrap();
    let UpgradeOutcome::Rejected(resp) = outcome else {
        panic!("expected rejection");
    };
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_string(resp).await.starts_with("GET /ws HTTP/1.1"));
}

#[tokio::test]
async fn invalid_upgrade_requests_never_leave() {
    let up = echo(Wire::Plain).await;
    let c = client();
    let post = Request::post("/ws")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .body(empty_body())
        .unwrap();
    assert_eq!(
        c.upgrade(&up.target(Wire::Plain), post)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidRequest
    );
    let h2c = Request::get("/")
        .header("upgrade", "h2c")
        .header("connection", "Upgrade, HTTP2-Settings")
        .body(empty_body())
        .unwrap();
    assert_eq!(
        c.upgrade(&up.target(Wire::Plain), h2c)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidRequest
    );
    assert_eq!(up.accepted(), 0);
}

#[tokio::test]
async fn http_version_of_response_is_reported() {
    let up = echo(Wire::TlsH2).await;
    let resp = client()
        .send(&up.target(Wire::TlsH2), get("/"))
        .await
        .unwrap();
    assert_eq!(resp.version(), Version::HTTP_2);
}

/// TLS upstream offering `h2` and `http/1.1` whose HTTP/2 side resets every
/// stream with `HTTP_1_1_REQUIRED` (counting them); its HTTP/1.1 side answers
/// `h1`, followed by `:` and the request body when there is one.
async fn h2_requires_h1() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(support::server_tls(&[b"h2", b"http/1.1"])));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let streams = Arc::new(AtomicUsize::new(0));
    let seen = streams.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (acceptor, streams) = (acceptor.clone(), streams.clone());
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
                    let Ok(mut conn) = h2::server::handshake(tls).await else {
                        return;
                    };
                    while let Some(Ok((_req, mut respond))) = conn.accept().await {
                        streams.fetch_add(1, Ordering::SeqCst);
                        respond.send_reset(h2::Reason::HTTP_1_1_REQUIRED);
                    }
                } else {
                    let svc = hyper::service::service_fn(|req: Request<Incoming>| async {
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        let answer = if body.is_empty() {
                            "h1".to_owned()
                        } else {
                            format!("h1:{}", String::from_utf8_lossy(&body))
                        };
                        Ok::<_, std::convert::Infallible>(text(answer))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                        .await;
                }
            });
        }
    });
    (addr, seen)
}

fn replayable_post(data: &'static [u8]) -> Request<toolkit_http_proxy::ProxyBody> {
    let data = Bytes::from_static(data);
    let mut post = Request::post("/").body(full_body(data.clone())).unwrap();
    post.extensions_mut()
        .insert(toolkit_http_proxy::ReplayableBody(data));
    post
}

#[tokio::test]
async fn http_1_1_required_falls_back_to_http1_for_the_host() {
    // Any method is sent again, once, with its whole body: servers send
    // this code before processing (ADR-0004, "Retry").
    let (addr, streams) = h2_requires_h1().await;
    let c = client();
    let target = Target::new(addr, "localhost", Transport::Tls);
    let resp = c
        .send(&target, replayable_post(b"payload"))
        .await
        .expect("resent over HTTP/1.1");
    assert_eq!(resp.version(), Version::HTTP_11);
    assert_eq!(body_string(resp).await, "h1:payload");
    assert_eq!(streams.load(Ordering::SeqCst), 1);
    assert_eq!(c.cached_protocol(&target), Some(CachedProtocol::Http1Only));

    // Later requests go straight to HTTP/1.1.
    let resp = c.send(&target, get("/")).await.unwrap();
    assert_eq!(resp.version(), Version::HTTP_11);
    assert_eq!(body_string(resp).await, "h1");
    assert_eq!(streams.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_1_1_required_falls_back_even_without_the_protocol_cache() {
    let (addr, streams) = h2_requires_h1().await;
    let mut cfg = test_config();
    cfg.protocol_cache_ttl = Duration::ZERO;
    let c = ProxyClient::new(cfg).unwrap();
    let target = Target::new(addr, "localhost", Transport::Tls);
    for _ in 0..2 {
        let resp = c
            .send(&target, replayable_post(b"payload"))
            .await
            .expect("resent over HTTP/1.1");
        assert_eq!(resp.version(), Version::HTTP_11);
        assert_eq!(body_string(resp).await, "h1:payload");
    }
    assert_eq!(streams.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_1_1_required_never_resends_a_streamed_body() {
    let (addr, streams) = h2_requires_h1().await;
    let c = client();
    let target = Target::new(addr, "localhost", Transport::Tls);
    let chunks = stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"payload"))]);
    let post = Request::post("/").body(stream_body(chunks)).unwrap();
    assert!(c.send(&target, post).await.is_err(), "the body is gone");
    assert_eq!(streams.load(Ordering::SeqCst), 1);

    // The host still moves to HTTP/1.1 for the next request.
    assert_eq!(c.cached_protocol(&target), Some(CachedProtocol::Http1Only));
    let resp = c.send(&target, get("/")).await.unwrap();
    assert_eq!(body_string(resp).await, "h1");
}

#[tokio::test]
async fn http_1_1_required_is_final_for_an_http2_only_target() {
    let (addr, streams) = h2_requires_h1().await;
    let c = client();
    let target =
        Target::new(addr, "localhost", Transport::Tls).with_policy(ProtocolPolicy::Http2Only);
    let res = c.send(&target, replayable_post(b"payload")).await;
    assert!(res.is_err(), "HTTP/1.1 is not allowed for this target");
    assert_eq!(streams.load(Ordering::SeqCst), 1);
}

/// TLS upstream offering `h2` and `http/1.1` whose HTTP/2 side answers the
/// connection preface with SETTINGS and `GOAWAY(HTTP_1_1_REQUIRED)` (never
/// acknowledging a PING); its HTTP/1.1 side answers `h1`.
async fn h2_goaway_requires_h1() -> std::net::SocketAddr {
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(support::server_tls(&[b"h2", b"http/1.1"])));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
                    let mut preface = [0u8; 24];
                    if tls.read_exact(&mut preface).await.is_err() {
                        return;
                    }
                    let settings = [0, 0, 0, 0x4, 0, 0, 0, 0, 0];
                    let goaway = [0, 0, 8, 0x7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0d];
                    let _ = tls.write_all(&settings).await;
                    let _ = tls.write_all(&goaway).await;
                    let _ = tls.flush().await;
                    let mut sink = [0u8; 1024];
                    while matches!(tls.read(&mut sink).await, Ok(n) if n > 0) {}
                } else {
                    let svc = hyper::service::service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(text("h1"))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                        .await;
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn goaway_http_1_1_required_during_setup_falls_back_to_http1() {
    let addr = h2_goaway_requires_h1().await;
    let c = client();
    let target = Target::new(addr, "localhost", Transport::Tls);
    let resp = tokio::time::timeout(Duration::from_secs(5), c.send(&target, get("/")))
        .await
        .expect("no hang")
        .expect("connected again over HTTP/1.1");
    assert_eq!(resp.version(), Version::HTTP_11);
    assert_eq!(body_string(resp).await, "h1");
    assert_eq!(c.cached_protocol(&target), Some(CachedProtocol::Http1Only));
}

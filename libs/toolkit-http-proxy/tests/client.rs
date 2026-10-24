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
use futures_util::stream;
use http::{Request, StatusCode, Version};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use support::{Wire, body_string, client, echo, test_config, text, upstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use toolkit_http_proxy::{
    CachedProtocol, ConnectionInfo, ErrorKind, Observer, ProxyClient, RootStore, Target, Transport,
    UpgradeOutcome, empty_body, full_body, stream_body,
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
    let c = client();
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

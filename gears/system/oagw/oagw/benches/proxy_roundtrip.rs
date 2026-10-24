//! Engine-agnostic data-plane round-trip benchmark.
//!
//! Drives requests through the full OAGW router (ingress handler → plugin
//! chain → upstream engine) against local hyper upstreams, so the same file
//! measures whichever proxy engine the data plane is built on. Reports
//! sequential latency percentiles and concurrent throughput per scenario.
//!
//! Run: `cargo bench -p cf-gears-oagw --bench proxy_roundtrip`
//! Tune with `OAGW_BENCH_ITERS` (sequential requests, default 2000) and
//! `OAGW_BENCH_SECS` (concurrent phase duration, default 3). Not meant for
//! `--features fips` builds: the harness wires a plaintext token client.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oagw::test_support::{AppHarness, test_server_config};
use oagw_sdk::HTTP_PROTOCOL_ID;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

const SMALL: usize = 1024;
const LARGE: usize = 256 * 1024;
const CONCURRENCY: usize = 64;

#[derive(Clone, Copy)]
enum Wire {
    H1,
    H2Tls,
}

/// Upstream that answers `GET /r/{n}` with `n` bytes and echoes the size of
/// any request body as a short JSON document.
async fn start_upstream(wire: Wire) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("local addr");
    let small = Bytes::from(vec![b'a'; SMALL]);
    let large = Bytes::from(vec![b'b'; LARGE]);
    let tls = matches!(wire, Wire::H2Tls).then(|| {
        let mut cfg = test_server_config();
        cfg.alpn_protocols = vec![b"h2".to_vec()];
        TlsAcceptor::from(Arc::new(cfg))
    });

    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                continue;
            };
            let _ = tcp.set_nodelay(true);
            let (small, large, tls) = (small.clone(), large.clone(), tls.clone());
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let (small, large) = (small.clone(), large.clone());
                    async move {
                        let path = req.uri().path().to_owned();
                        let received = req.into_body().collect().await?.to_bytes().len();
                        let body = if path.ends_with("/large") {
                            large
                        } else if path.ends_with("/small") {
                            small
                        } else {
                            Bytes::from(format!("{{\"received\":{received}}}"))
                        };
                        Ok::<_, hyper::Error>(Response::new(Full::new(body)))
                    }
                });
                let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                match tls {
                    Some(acceptor) => {
                        if let Ok(stream) = acceptor.accept(tcp).await {
                            let _ = builder
                                .http2_only()
                                .serve_connection(TokioIo::new(stream), svc)
                                .await;
                        }
                    }
                    None => {
                        let _ = builder
                            .http1_only()
                            .serve_connection(TokioIo::new(tcp), svc)
                            .await;
                    }
                }
            });
        }
    });
    addr
}

async fn register(h: &AppHarness, alias: &str, addr: SocketAddr, scheme: &str) {
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {"endpoints": [{"host": "127.0.0.1", "port": addr.port(), "scheme": scheme}]},
            "protocol": HTTP_PROTOCOL_ID,
            "alias": alias,
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().expect("upstream id").to_string();
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_id,
            "match": {"http": {"methods": ["GET", "POST"], "path": "/r"}},
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;
}

fn build_request(alias: &str, case: &str, body: &Bytes) -> http::Request<axum::body::Body> {
    let builder = http::Request::builder();
    let builder = if body.is_empty() {
        builder.method(http::Method::GET)
    } else {
        builder
            .method(http::Method::POST)
            .header(http::header::CONTENT_TYPE, "application/octet-stream")
    };
    builder
        .uri(format!("/oagw/v1/proxy/{alias}/r/{case}"))
        .body(axum::body::Body::from(body.clone()))
        .expect("request")
}

async fn one(router: &axum::Router, alias: &str, case: &str, body: &Bytes) -> usize {
    let resp = router
        .clone()
        .oneshot(build_request(alias, case, body))
        .await
        .expect("router");
    assert!(resp.status().is_success(), "status {}", resp.status());
    resp.into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .len()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

async fn run_case(
    router: &axum::Router,
    alias: &str,
    case: &str,
    body: Bytes,
    iters: usize,
    secs: u64,
) {
    for _ in 0..200 {
        one(router, alias, case, &body).await;
    }
    let mut lat = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        one(router, alias, case, &body).await;
        lat.push(t.elapsed());
    }
    lat.sort_unstable();

    let done = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut tasks = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let (router, alias, case, body, done) = (
            router.clone(),
            alias.to_owned(),
            case.to_owned(),
            body.clone(),
            done.clone(),
        );
        tasks.push(tokio::spawn(async move {
            while Instant::now() < deadline {
                one(&router, &alias, &case, &body).await;
                done.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for t in tasks {
        t.await.expect("worker");
    }
    let rps = done.load(Ordering::Relaxed) as f64 / secs as f64;

    println!(
        "{alias:<6} {case:<6} body={:>7}B  p50={:>9.1?}  p90={:>9.1?}  p99={:>9.1?}  c{CONCURRENCY}={rps:>9.0} req/s",
        body.len(),
        percentile(&lat, 0.50),
        percentile(&lat, 0.90),
        percentile(&lat, 0.99),
    );
}

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let iters = usize::try_from(env_or("OAGW_BENCH_ITERS", 2000)).unwrap_or(2000);
    let secs = env_or("OAGW_BENCH_SECS", 3);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let _ = toolkit::bootstrap::init_crypto_provider();
        let h1 = start_upstream(Wire::H1).await;
        let h2 = start_upstream(Wire::H2Tls).await;
        let h = AppHarness::builder()
            .with_skip_upstream_tls_verify(true)
            .build()
            .await;
        register(&h, "h1", h1, "http").await;
        register(&h, "h2tls", h2, "https").await;
        let router = h.router().clone();
        let post = Bytes::from(vec![b'p'; 64 * 1024]);
        for alias in ["h1", "h2tls"] {
            run_case(&router, alias, "small", Bytes::new(), iters, secs).await;
            run_case(&router, alias, "large", Bytes::new(), iters, secs).await;
            run_case(&router, alias, "echo", post.clone(), iters, secs).await;
        }
    });
}

//! Engine-only latency/throughput against local upstreams.
//!
//! `cargo bench -p cf-gears-toolkit-http-proxy --bench client`
//! (`PROXY_BENCH_ITERS`, `PROXY_BENCH_SECS` tune the run).

// A benchmark harness, not library code: failing loudly is the point.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::let_underscore_must_use,
    clippy::use_debug
)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::Request;
use http_body_util::BodyExt;
use support::{Wire, text, upstream};
use toolkit_http_proxy::{ProxyClient, Target, full_body};

const CONCURRENCY: usize = 64;

async fn one(client: &ProxyClient, target: &Target, path: &str) {
    let req = Request::get(path)
        .body(full_body(Bytes::new()))
        .expect("request");
    let resp = client.send(target, req).await.expect("send");
    let _ = resp.into_body().collect().await.expect("body");
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

async fn run(
    client: &ProxyClient,
    name: &str,
    target: Target,
    path: &str,
    iters: usize,
    secs: u64,
) {
    for _ in 0..200 {
        one(client, &target, path).await;
    }
    let mut lat = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        one(client, &target, path).await;
        lat.push(t.elapsed());
    }
    lat.sort_unstable();
    let done = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(secs);
    let workers: Vec<_> = (0..CONCURRENCY)
        .map(|_| {
            let (client, target, done, path) = (
                client.clone(),
                target.clone(),
                done.clone(),
                path.to_owned(),
            );
            tokio::spawn(async move {
                while Instant::now() < deadline {
                    one(&client, &target, &path).await;
                    done.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for w in workers {
        w.await.expect("worker");
    }
    #[allow(clippy::cast_precision_loss)]
    let rps = done.load(Ordering::Relaxed) as f64 / secs as f64;
    println!(
        "{name:<12} {path:<7} p50={:>9.1?} p90={:>9.1?} p99={:>9.1?} c{CONCURRENCY}={rps:>9.0} req/s",
        pct(&lat, 0.5),
        pct(&lat, 0.9),
        pct(&lat, 0.99)
    );
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let iters = usize::try_from(env("PROXY_BENCH_ITERS", 2000)).unwrap_or(2000);
    let secs = env("PROXY_BENCH_SECS", 3);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        let small = Bytes::from(vec![b'a'; 1024]);
        let large = Bytes::from(vec![b'b'; 256 * 1024]);
        let handler = move |req: hyper::Request<hyper::body::Incoming>| {
            let (small, large) = (small.clone(), large.clone());
            async move {
                text(if req.uri().path() == "/large" {
                    large
                } else {
                    small
                })
            }
        };
        let client = support::client();
        for (name, wire) in [
            ("h1", Wire::Plain),
            ("h1-tls", Wire::TlsH1),
            ("h2-tls", Wire::TlsH2),
        ] {
            let up = upstream(wire, handler.clone()).await;
            let target = up.target(wire);
            run(&client, name, target.clone(), "/small", iters, secs).await;
            run(&client, name, target, "/large", iters, secs).await;
        }
    });
}

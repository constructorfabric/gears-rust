//! `ManualCallContext::send_raw` through a `#[passthrough]` method (ADR-0009 §4).

#![allow(clippy::unwrap_used)]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::any;
use bytes::Bytes;
use http_body::Frame;
use http_body_util::{BodyExt as _, StreamBody};
use secrecy::SecretString;
use toolkit_contract::runtime::config::{ClientConfig, InternalTokenProvider};
use toolkit_contract::runtime::raw::{empty_body, full_body, stream_body};
use toolkit_contract_manual_fixture::{ManualDemoApi, ManualDemoApiRestClient, RawBody};
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

#[derive(Clone, Default)]
struct Upstream {
    hits: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
}

fn seen(req: &Request, name: &str) -> String {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned()
}

async fn upstream(State(state): State<Upstream>, req: Request) -> Response {
    state.hits.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_owned();
    let tail = path
        .trim_start_matches("/manual-demo/v1/proxy/")
        .trim_start_matches("/manual-demo/v1/platform-proxy/");
    let frames = |frames: Vec<Result<Frame<Bytes>, std::io::Error>>| {
        Body::new(StreamBody::new(futures_util::stream::iter(frames)))
    };
    match tail {
        "trailers" => {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("x-checksum", "abc".parse().unwrap());
            // hyper sends only the trailer fields the head declares.
            Response::builder()
                .header("trailer", "x-checksum")
                .body(frames(vec![
                    Ok(Frame::data(Bytes::from_static(b"with "))),
                    Ok(Frame::data(Bytes::from_static(b"trailers"))),
                    Ok(Frame::trailers(trailers)),
                ]))
                .unwrap()
        }
        "gzip" => Response::builder()
            .header("content-encoding", "gzip")
            .body(Body::from(vec![0x1f, 0x8b, 0x08, 0x00]))
            .unwrap(),
        "redirect" => Response::builder()
            .status(302)
            .header("location", "http://elsewhere.invalid/")
            .body(Body::empty())
            .unwrap(),
        "slow" => {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Response::new(Body::empty())
        }
        // The head at once, the body a second later.
        "stall" => Response::new(Body::new(StreamBody::new(futures_util::stream::once(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"late")))
            },
        )))),
        // Three chunks 100 ms apart.
        "drip" => Response::new(Body::new(StreamBody::new(futures_util::stream::unfold(
            0u8,
            |n| async move {
                if n == 3 {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                let chunk = Bytes::from(n.to_string());
                Some((Ok::<_, Infallible>(Frame::data(chunk)), n + 1))
            },
        )))),
        "broken" => {
            // Fail only after the head and a first chunk went out.
            let first =
                futures_util::stream::iter([Ok(Frame::data(Bytes::from_static(b"partial")))]);
            let failure = futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Err(std::io::Error::other("upstream died"))
            });
            Response::new(Body::new(StreamBody::new(futures_util::StreamExt::chain(
                first, failure,
            ))))
        }
        "hold" => {
            let release = Arc::clone(&state.release);
            let first = futures_util::stream::iter([Ok::<_, Infallible>(Frame::data(
                Bytes::from_static(b"first"),
            ))]);
            let second = futures_util::stream::once(async move {
                release.notified().await;
                Ok(Frame::data(Bytes::from_static(b"second")))
            });
            Response::new(Body::new(StreamBody::new(futures_util::StreamExt::chain(
                first, second,
            ))))
        }
        _ => {
            let status: u16 = seen(&req, "x-status").parse().unwrap_or(200);
            let response = Response::builder()
                .status(status)
                .header("x-seen-method", req.method().as_str())
                .header(
                    "x-seen-path",
                    req.uri().path_and_query().map_or("", |pq| pq.as_str()),
                )
                .header("x-seen-host", seen(&req, "host"))
                .header("x-seen-authorization", seen(&req, "authorization"))
                .header(
                    "x-seen-internal-token",
                    seen(&req, "x-toolkit-internal-token"),
                )
                .header(
                    "x-seen-custom",
                    req.headers().get_all("x-custom").iter().count(),
                )
                .header("x-multi", "a")
                .header("x-multi", "b");
            let body = req.into_body().collect().await.unwrap().to_bytes();
            response.body(Body::from(body)).unwrap()
        }
    }
}

async fn start() -> (String, Upstream) {
    let state = Upstream::default();
    let app = axum::Router::new()
        .route("/manual-demo/v1/proxy/{*path}", any(upstream))
        .route("/manual-demo/v1/platform-proxy/{*path}", any(upstream))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn client(url: &str) -> ManualDemoApiRestClient {
    ManualDemoApiRestClient::new(ClientConfig::new(url)).unwrap()
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::nil())
        .subject_tenant_id(uuid::Uuid::nil())
        .bearer_token("tenant.jwt".to_owned())
        .build()
        .unwrap()
}

fn header(resp: &http::Response<RawBody>, name: &str) -> String {
    resp.headers()[name].to_str().unwrap().to_owned()
}

async fn body(resp: http::Response<RawBody>) -> Bytes {
    resp.into_body().collect().await.unwrap().to_bytes()
}

#[tokio::test]
async fn path_query_bytes_headers_and_status_survive() {
    let (url, _) = start().await;
    let req = http::Request::put("/alias/a%20b/c?q=1&q=2")
        .header("x-status", "503")
        .header("x-custom", "1")
        .header("x-custom", "2")
        .body(full_body(Bytes::from_static(b"payload")))
        .unwrap();

    let resp = client(&url).proxy(ctx(), req).await.unwrap();

    assert_eq!(resp.status(), 503, "non-2xx is returned, not an error");
    assert_eq!(header(&resp, "x-seen-method"), "PUT");
    assert_eq!(
        header(&resp, "x-seen-path"),
        "/manual-demo/v1/proxy/alias/a%20b/c?q=1&q=2"
    );
    assert_eq!(header(&resp, "x-seen-custom"), "2");
    let multi: Vec<_> = resp.headers().get_all("x-multi").iter().collect();
    assert_eq!(multi, ["a", "b"]);
    assert_eq!(body(resp).await, "payload");
}

#[tokio::test]
async fn caller_cannot_redirect_the_call_or_its_credentials() {
    let (url, _) = start().await;
    let req = http::Request::get("http://evil.example/alias/x")
        .header("host", "evil.example")
        // Nominating the credential headers must not strip the real ones.
        .header("connection", "authorization, x-toolkit-internal-token")
        .header("authorization", "Bearer caller")
        .header("x-toolkit-internal-token", "forged")
        .body(empty_body())
        .unwrap();

    let resp = client(&url).proxy(ctx(), req).await.unwrap();

    assert_eq!(
        header(&resp, "x-seen-host"),
        url.trim_start_matches("http://")
    );
    assert_eq!(header(&resp, "x-seen-authorization"), "Bearer tenant.jwt");
    assert_eq!(header(&resp, "x-seen-internal-token"), "-");
}

#[tokio::test]
async fn streamed_upload_arrives_whole() {
    let (url, _) = start().await;
    let chunks = futures_util::stream::iter(
        ["one ", "two ", "three"]
            .map(|c| Ok::<_, std::io::Error>(Bytes::from_static(c.as_bytes()))),
    );
    let req = http::Request::post("/upload")
        .body(stream_body(chunks))
        .unwrap();

    let resp = client(&url).proxy(ctx(), req).await.unwrap();
    assert_eq!(body(resp).await, "one two three");
}

#[tokio::test]
async fn trailers_reach_the_caller() {
    let (url, _) = start().await;
    let req = http::Request::get("/trailers")
        .header("te", "trailers")
        .body(empty_body())
        .unwrap();

    let resp = client(&url).proxy(ctx(), req).await.unwrap();
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().cloned().expect("trailers");
    assert_eq!(trailers["x-checksum"], "abc");
    assert_eq!(collected.to_bytes(), "with trailers");
}

#[tokio::test]
async fn no_decompression_and_no_redirects() {
    let (url, _) = start().await;
    let api = client(&url);

    let gzip = api
        .proxy(
            ctx(),
            http::Request::get("/gzip").body(empty_body()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(header(&gzip, "content-encoding"), "gzip");
    assert_eq!(
        body(gzip).await,
        Bytes::from_static(&[0x1f, 0x8b, 0x08, 0x00])
    );

    let redirect = api
        .proxy(
            ctx(),
            http::Request::get("/redirect").body(empty_body()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(redirect.status(), 302);
}

#[tokio::test]
async fn failure_after_the_head_is_a_body_error() {
    let (url, _) = start().await;
    let resp = client(&url)
        .proxy(
            ctx(),
            http::Request::get("/broken").body(empty_body()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.into_body().collect().await.unwrap_err();
}

#[tokio::test]
async fn upload_slower_than_the_open_timeout_completes() {
    // `timeout` bounds opening the exchange, not the request body: three
    // chunks 150 ms apart take longer than the 200 ms open timeout.
    let (url, _) = start().await;
    let api = ManualDemoApiRestClient::new(
        ClientConfig::new(url).with_timeout(Duration::from_millis(200)),
    )
    .unwrap();
    let chunks = futures_util::stream::unfold(0u8, |n| async move {
        if n == 3 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        Some((Ok::<_, std::io::Error>(Bytes::from(n.to_string())), n + 1))
    });
    let req = http::Request::post("/upload")
        .body(stream_body(chunks))
        .unwrap();

    let resp = api.proxy(ctx(), req).await.unwrap();
    assert_eq!(body(resp).await, "012");
}

#[tokio::test]
async fn response_body_streams_before_the_upstream_finishes() {
    let (url, upstream) = start().await;
    let resp = client(&url)
        .proxy(
            ctx(),
            http::Request::get("/hold").body(empty_body()).unwrap(),
        )
        .await
        .unwrap();
    let mut body = resp.into_body();

    // The upstream holds its second chunk until released, so the first one
    // can only arrive if the body is not buffered.
    let first = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("first chunk before the upstream finished")
        .unwrap()
        .unwrap();
    assert_eq!(first.into_data().unwrap(), "first");

    upstream.release.notify_one();
    let remainder = body.collect().await.unwrap().to_bytes();
    assert_eq!(remainder, "second");
}

#[tokio::test]
async fn unforwardable_requests_never_reach_the_upstream() {
    let (url, upstream) = start().await;
    let api = client(&url);
    let requests = [
        http::Request::get("/ws")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .body(empty_body())
            .unwrap(),
        http::Request::connect("example.com:443")
            .body(empty_body())
            .unwrap(),
        // PATCH is not in the binding's method set.
        http::Request::patch("/alias").body(empty_body()).unwrap(),
        http::Request::get("/alias/../admin")
            .body(empty_body())
            .unwrap(),
        http::Request::get("/alias/%2e%2e/admin")
            .body(empty_body())
            .unwrap(),
    ];
    for req in requests {
        let description = format!("{} {}", req.method(), req.uri());
        assert!(
            api.proxy(ctx(), req).await.is_err(),
            "{description} must be refused"
        );
    }
    assert_eq!(upstream.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn raw_and_generated_calls_share_one_concurrency_budget() {
    let (url, _) = start().await;
    let api = Arc::new(
        ManualDemoApiRestClient::new(ClientConfig::new(url).with_max_concurrent_requests(Some(1)))
            .unwrap(),
    );

    // A raw call waiting for its response head holds the only slot...
    let slow = tokio::spawn({
        let api = Arc::clone(&api);
        async move {
            api.proxy(
                ctx(),
                http::Request::get("/slow").body(empty_body()).unwrap(),
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ...so a generated call is shed before it reaches the network.
    let shed = api.echo(ctx(), "x".to_owned()).await.unwrap_err();
    assert!(
        format!("{shed:?}").contains("concurrency limit"),
        "{shed:?}"
    );

    slow.await.unwrap().unwrap();
}

#[tokio::test]
async fn name_resolution_counts_against_the_open_timeout() {
    // `.invalid` never resolves; the resolver retries for about three seconds.
    let api = ManualDemoApiRestClient::new(
        ClientConfig::new("http://provider.invalid").with_timeout(Duration::from_millis(200)),
    )
    .unwrap();

    let started = Instant::now();
    let err = api
        .proxy(ctx(), http::Request::get("/x").body(empty_body()).unwrap())
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(format!("{err:?}").contains("timeout"), "{err:?}");
}

#[tokio::test]
async fn every_resolved_address_is_tried() {
    // `localhost` usually resolves to `::1` before `127.0.0.1`; the server
    // listens on IPv4 only, so a call succeeds only if the next address is
    // tried after the first one refuses.
    let (url, _) = start().await;
    let port = url.rsplit(':').next().unwrap();
    let resp = client(&format!("http://localhost:{port}"))
        .proxy(
            ctx(),
            http::Request::get("/alias").body(empty_body()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

fn get(path: &str) -> http::Request<RawBody> {
    http::Request::get(path).body(empty_body()).unwrap()
}

#[tokio::test]
async fn idle_budget_bounds_the_wait_for_the_response_head() {
    // reqwest `read_timeout_applies_to_headers`: the open timeout is long, but
    // no frame arrives within `stream_idle_timeout`.
    let (url, _) = start().await;
    let api = ManualDemoApiRestClient::new(
        ClientConfig::new(url).with_stream_idle_timeout(Duration::from_millis(200)),
    )
    .unwrap();

    let started = Instant::now();
    let err = api.proxy(ctx(), get("/slow")).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(format!("{err:?}").contains("timeout"), "{err:?}");
}

#[tokio::test]
async fn stalled_response_body_fails_instead_of_hanging() {
    // reqwest `read_timeout_applies_to_body`.
    let (url, _) = start().await;
    let api = ManualDemoApiRestClient::new(
        ClientConfig::new(url).with_stream_idle_timeout(Duration::from_millis(200)),
    )
    .unwrap();

    let resp = api.proxy(ctx(), get("/stall")).await.unwrap();
    assert_eq!(resp.status(), 200);
    resp.into_body().collect().await.unwrap_err();
}

#[tokio::test]
async fn open_timeout_does_not_cut_a_streaming_body() {
    // reqwest `read_timeout_allows_slow_response_body`: the body takes longer
    // than `timeout` in total, but every chunk arrives within the idle budget.
    let (url, _) = start().await;
    let api = ManualDemoApiRestClient::new(
        ClientConfig::new(url).with_timeout(Duration::from_millis(150)),
    )
    .unwrap();

    let resp = api.proxy(ctx(), get("/drip")).await.unwrap();
    assert_eq!(body(resp).await, "012");
}

#[tokio::test]
async fn tls_policy_and_unknown_schemes_are_refused_before_sending() {
    // reqwest `https_only` / `test_invalid_scheme_is_rejected`.
    let (url, upstream) = start().await;
    let tls_only =
        ManualDemoApiRestClient::new(ClientConfig::new(&url).with_require_tls(true)).unwrap();
    tls_only.proxy(ctx(), get("/alias")).await.unwrap_err();

    let ftp = client(&url.replacen("http://", "ftp://", 1));
    ftp.proxy(ctx(), get("/alias")).await.unwrap_err();

    assert_eq!(upstream.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn raw_calls_are_shed_when_the_budget_is_taken() {
    let (url, _) = start().await;
    let api = Arc::new(
        ManualDemoApiRestClient::new(ClientConfig::new(url).with_max_concurrent_requests(Some(1)))
            .unwrap(),
    );
    let slow = tokio::spawn({
        let api = Arc::clone(&api);
        async move { api.proxy(ctx(), get("/slow")).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let shed = api.proxy(ctx(), get("/alias")).await.unwrap_err();
    assert!(
        format!("{shed:?}").contains("concurrency limit"),
        "{shed:?}"
    );

    slow.await.unwrap().unwrap();
}

#[tokio::test]
async fn unreachable_provider_is_a_network_error() {
    let port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let started = Instant::now();
    let err = client(&format!("http://127.0.0.1:{port}"))
        .proxy(ctx(), get("/alias"))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!format!("{err:?}").contains("timeout"), "{err:?}");
}

#[tokio::test]
async fn credential_that_is_not_a_header_value_fails_closed() {
    let (url, upstream) = start().await;
    let ctx = SecurityContext::builder()
        .subject_id(uuid::Uuid::nil())
        .subject_tenant_id(uuid::Uuid::nil())
        .bearer_token("bad\ntoken".to_owned())
        .build()
        .unwrap();

    client(&url).proxy(ctx, get("/alias")).await.unwrap_err();
    assert_eq!(upstream.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn platform_plane_sends_the_runtime_token_not_the_caller_headers() {
    let (url, _) = start().await;
    let api = ManualDemoApiRestClient::new(ClientConfig::new(url).with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt")),
    ))
    .unwrap();
    let req = http::Request::get("/alias")
        .header("authorization", "Bearer caller")
        .header("x-toolkit-internal-token", "forged")
        .body(empty_body())
        .unwrap();
    let platform = PlatformSecurityContext::new(PlatformIdentity::Shared {
        name: "caller".to_owned(),
    });

    let resp = api.platform_proxy(platform, req).await.unwrap();
    assert_eq!(header(&resp, "x-seen-internal-token"), "sa.jwt");
    assert_eq!(header(&resp, "x-seen-authorization"), "-");
}

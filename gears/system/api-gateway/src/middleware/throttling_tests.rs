//! Unit tests for the zone-based throttling middleware.

use super::*;
use crate::config::{KeyConfig, RateSpec};
use axum::Router;
use axum::body::Body;
use axum::routing::get;
use std::time::Duration;
use tower::ServiceExt;

use toolkit::api::operation_builder::VendorExtensions;

fn op(method: Method, path: &str, throttling: Option<ThrottlingSpec>) -> OperationSpec {
    OperationSpec {
        method,
        path: path.to_owned(),
        operation_id: None,
        summary: None,
        description: None,
        tags: vec![],
        params: vec![],
        request_body: None,
        responses: vec![],
        handler_id: "test".to_owned(),
        // Identity zones need an authenticated operation; IP zones do not care.
        auth: if throttling
            .as_ref()
            .is_some_and(|t| t.require_security_context)
        {
            toolkit::api::RouteAuth::Authenticated
        } else {
            toolkit::api::RouteAuth::Anonymous
        },
        exposed: true,
        throttling,
        allowed_request_content_types: None,
        vendor_extensions: VendorExtensions::default(),
        license_requirement: None,
    }
}

/// Map a test zone argument to `Option<String>`, treating `""` as "no zone".
fn zone(name: &str) -> Option<String> {
    (!name.is_empty()).then(|| name.to_owned())
}

fn thr(rate_zone: &str, inflight_zone: &str, require_ctx: bool) -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: zone(rate_zone),
        in_flight_limit_zone: zone(inflight_zone),
        require_security_context: require_ctx,
        dry_run: false,
    }
}

fn thr_dry(rate_zone: &str, inflight_zone: &str) -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: zone(rate_zone),
        in_flight_limit_zone: zone(inflight_zone),
        require_security_context: false,
        dry_run: true,
    }
}

fn rate_zone_cfg(rps: u32, burst: u32, key: KeyType) -> RateLimitZone {
    RateLimitZone {
        rate_limit: RateSpec { rps },
        burst_limit: burst,
        response_status_code: 429,
        response_retry_after: RetryAfter::Auto,
        key: KeyConfig { key_type: key },
        max_keys: 1000,
    }
}

fn inflight_zone_cfg(in_flight: u32, key: KeyType, excluded: Vec<String>) -> InFlightLimitZone {
    InFlightLimitZone {
        in_flight_limit: in_flight,
        backlog_limit: 0,
        backlog_timeout: Duration::from_millis(50),
        response_status_code: 429,
        key: KeyConfig { key_type: key },
        max_keys: 1000,
        excluded_keys: excluded,
    }
}

fn cfg_with_rate(name: &str, zone: RateLimitZone) -> ApiGatewayConfig {
    let mut cfg = ApiGatewayConfig::default();
    cfg.rate_limit_zones.insert(name.to_owned(), zone);
    cfg
}

#[test]
fn partitions_specs_by_require_security_context() {
    let mut cfg = ApiGatewayConfig::default();
    cfg.rate_limit_zones
        .insert("ip".to_owned(), rate_zone_cfg(10, 10, KeyType::Ip));
    cfg.rate_limit_zones
        .insert("id".to_owned(), rate_zone_cfg(10, 10, KeyType::Identity));

    let specs = vec![
        op(Method::GET, "/pre", Some(thr("ip", "", false))),
        op(Method::GET, "/post", Some(thr("id", "", true))),
        op(Method::GET, "/none", None),
    ];

    let pre = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let post = ThrottlingMap::from_specs(&specs, &cfg).unwrap();

    assert_eq!(pre.inner.routes.len(), 1);
    assert!(
        pre.inner
            .routes
            .contains_key(&(Method::GET, "/pre".to_owned()))
    );
    assert_eq!(post.inner.routes.len(), 1);
    assert!(
        post.inner
            .routes
            .contains_key(&(Method::GET, "/post".to_owned()))
    );
}

#[test]
fn pre_auth_identity_zone_is_rejected() {
    let cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    let specs = vec![op(Method::GET, "/x", Some(thr("id", "", false)))];
    let err = ThrottlingMapNoAuth::from_specs(&specs, &cfg)
        .err()
        .expect("should error")
        .to_string();
    assert!(
        err.contains("identity keying requires authentication"),
        "{err}"
    );
}

#[test]
fn undefined_zone_is_rejected() {
    let cfg = ApiGatewayConfig::default();
    let specs = vec![op(Method::GET, "/x", Some(thr("missing", "", false)))];
    let err = ThrottlingMapNoAuth::from_specs(&specs, &cfg)
        .err()
        .expect("should error")
        .to_string();
    assert!(err.contains("undefined rate_limit zone"), "{err}");
}

#[test]
fn shared_zone_arc_within_map() {
    let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
    let specs = vec![
        op(Method::GET, "/a", Some(thr("ip", "", false))),
        op(Method::GET, "/b", Some(thr("ip", "", false))),
    ];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let a = map.inner.routes[&(Method::GET, "/a".to_owned())]
        .rate_zone
        .clone()
        .unwrap();
    let b = map.inner.routes[&(Method::GET, "/b".to_owned())]
        .rate_zone
        .clone()
        .unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn shared_zone_arc_across_partitions() {
    // The same IP-keyed zone referenced by a pre-auth and a post-auth
    // operation must resolve to a single limiter instance.
    let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
    let specs = vec![
        op(Method::GET, "/pre", Some(thr("ip", "", false))),
        op(Method::GET, "/post", Some(thr("ip", "", true))),
    ];
    let (auth, noauth, _pruner) = build_maps(&specs, &cfg).unwrap();
    let pre = noauth.inner.routes[&(Method::GET, "/pre".to_owned())]
        .rate_zone
        .clone()
        .unwrap();
    let post = auth.inner.routes[&(Method::GET, "/post".to_owned())]
        .rate_zone
        .clone()
        .unwrap();
    assert!(Arc::ptr_eq(&pre, &post));
}

#[test]
fn client_ip_ignores_forwarding_headers_without_trusted_proxies() {
    // With trusted_proxy_hops = 0, client-supplied headers must be ignored
    // and the peer address used, so a caller cannot spoof the bucket key.
    let mut req = Request::builder()
        .header("x-forwarded-for", "203.0.113.7, 10.0.0.1")
        .header("x-real-ip", "198.51.100.9")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.168.1.5:1234".parse::<SocketAddr>().unwrap(),
    ));
    assert_eq!(client_ip(&req, 0), "192.168.1.5");

    // No peer address either → "unknown".
    let req = Request::builder()
        .header("x-forwarded-for", "203.0.113.7")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req, 0), "unknown");
}

#[test]
fn client_ip_uses_trusted_proxy_hop() {
    // One trusted proxy: the rightmost XFF entry is the peer-observed client
    // (or a spoofed prefix shifts the trusted index right, never affecting it).
    let req = Request::builder()
        .header("x-forwarded-for", "203.0.113.7")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req, 1), "203.0.113.7");

    // A spoofed leftmost entry is ignored; the trusted (rightmost) hop wins.
    let req = Request::builder()
        .header("x-forwarded-for", "1.1.1.1, 203.0.113.7")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req, 1), "203.0.113.7");

    // Two trusted proxies: pick the entry two from the right.
    let req = Request::builder()
        .header("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req, 2), "203.0.113.7");
}

#[test]
fn client_ip_trusted_proxy_falls_back_when_xff_short_or_invalid() {
    // Fewer XFF entries than trusted hops → fall back to X-Real-IP.
    let req = Request::builder()
        .header("x-forwarded-for", "203.0.113.7")
        .header("x-real-ip", "198.51.100.9")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req, 3), "198.51.100.9");

    // Non-IP XFF token → fall back to peer address.
    let mut req = Request::builder()
        .header("x-forwarded-for", "not-an-ip")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "192.168.1.5:1234".parse::<SocketAddr>().unwrap(),
    ));
    assert_eq!(client_ip(&req, 1), "192.168.1.5");
}

#[test]
fn compute_key_identity_requires_security_context() {
    // No SecurityContext present → None (invariant violation, not "anonymous").
    let req = Request::builder().body(Body::empty()).unwrap();
    assert_eq!(
        compute_key(KeyType::Identity, RouteAuth::Authenticated, &req, 0),
        None
    );
    // IP keys never depend on a context.
    assert_eq!(
        compute_key(KeyType::Ip, RouteAuth::Authenticated, &req, 0).as_deref(),
        Some("unknown")
    );
}

#[test]
fn compute_key_identity_returns_subject_id() {
    let sc = SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(42))
        .subject_tenant_id(uuid::Uuid::from_u128(1))
        .build()
        .unwrap();
    let mut req = Request::builder().body(Body::empty()).unwrap();
    req.extensions_mut().insert(sc);
    assert_eq!(
        compute_key(KeyType::Identity, RouteAuth::Authenticated, &req, 0),
        Some("00000000-0000-0000-0000-00000000002a".to_owned())
    );
}

#[test]
fn identity_zone_rejects_anonymous_operation_and_auth_disabled() {
    let mut cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    let mut anon = op(Method::GET, "/x", Some(thr("id", "", true)));
    anon.auth = toolkit::api::RouteAuth::Anonymous;
    let err = ThrottlingMap::from_specs(&[anon], &cfg)
        .err()
        .expect("should error")
        .to_string();
    assert!(err.contains("allows anonymous access"), "{err}");

    cfg.auth_disabled = true;
    let authed = op(Method::GET, "/x", Some(thr("id", "", true)));
    let err = ThrottlingMap::from_specs(&[authed], &cfg)
        .err()
        .expect("should error")
        .to_string();
    assert!(err.contains("auth_disabled=true"), "{err}");
}

#[tokio::test]
async fn identity_zone_without_context_is_internal_error() {
    let cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    let specs = vec![op(Method::GET, "/x", Some(thr("id", "", true)))];
    let map = ThrottlingMap::from_specs(&specs, &cfg).unwrap();
    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_middleware(map, req, next).await }
            },
        ));
    // No SecurityContext in extensions: the zone invariant is violated.
    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn mixed_dry_run_and_enforce_on_one_zone_is_rejected() {
    let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
    let mixed = vec![
        op(Method::GET, "/a", Some(thr("ip", "", false))),
        op(Method::GET, "/b", Some(thr_dry("ip", ""))),
    ];
    let err = build_maps(&mixed, &cfg)
        .err()
        .expect("should error")
        .to_string();
    assert!(err.contains("all dry-run or all enforced"), "{err}");

    let same = vec![
        op(Method::GET, "/a", Some(thr_dry("ip", ""))),
        op(Method::GET, "/b", Some(thr_dry("ip", ""))),
    ];
    assert!(build_maps(&same, &cfg).is_ok());
}

#[tokio::test]
async fn retry_after_rounds_subsecond_wait_up() {
    // rps=2, burst=1: the wait after one hit is ~500 ms, the header must say 1.
    let cfg = cfg_with_rate("ip", rate_zone_cfg(2, 1, KeyType::Ip));
    let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));
    let req = || Request::builder().uri("/x").body(Body::empty()).unwrap();
    assert_eq!(
        app.clone().oneshot(req()).await.unwrap().status(),
        StatusCode::OK
    );
    let second = app.oneshot(req()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );
}

#[tokio::test]
async fn dry_run_in_flight_does_not_wait_for_backlog() {
    // in_flight_limit=1 held by a test handle, backlog would wait 500 ms:
    // dry-run must serve at once instead.
    let mut zone = inflight_zone_cfg(1, KeyType::Ip, vec![]);
    zone.backlog_limit = 1;
    zone.backlog_timeout = Duration::from_millis(500);
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert("ifl".to_owned(), zone);
    let specs = vec![op(Method::GET, "/x", Some(thr_dry("", "ifl")))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let inflight_zone = Arc::clone(
        map.inner.routes[&(Method::GET, "/x".to_owned())]
            .inflight_zone
            .as_ref()
            .unwrap(),
    );
    let held = inflight_zone
        .gate("unknown")
        .unwrap()
        .try_acquire()
        .unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));
    let started = std::time::Instant::now();
    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "dry-run must not wait in the backlog"
    );
    drop(held);
}

#[tokio::test]
async fn rate_limit_denies_after_burst() {
    let cfg = cfg_with_rate("ip", rate_zone_cfg(1, 1, KeyType::Ip));
    let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let first = app
        .clone()
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key(header::RETRY_AFTER));
    // Rate-limit rejections echo the policy/limit headers (legacy parity).
    assert!(second.headers().contains_key("RateLimit-Policy"));
    assert!(second.headers().contains_key("RateLimit-Limit"));
    assert!(second.headers().contains_key("X-RateLimit-Limit"));
}

#[test]
fn rate_zone_admit_caps_distinct_keys_until_reset() {
    let mut zones = HashMap::new();
    let mut cfg = rate_zone_cfg(1000, 1000, KeyType::Ip);
    cfg.max_keys = 2;
    let zone = get_or_build_rate_zone(&mut zones, "z", &cfg).unwrap();

    assert!(zone.admit("a"));
    assert!(zone.admit("b"));
    assert_eq!(zone.admitted_len.load(Ordering::Relaxed), 2);
    // Saturated: a new key is refused, already-admitted keys still pass.
    assert!(!zone.admit("c"));
    assert!(zone.admit("a"));
    assert_eq!(zone.admitted_len.load(Ordering::Relaxed), 2);

    // The prune resets admission; freed capacity is admittable again.
    zone.reset_admitted();
    assert_eq!(zone.admitted_len.load(Ordering::Relaxed), 0);
    assert!(zone.admit("c"));
}

#[tokio::test]
async fn rate_limit_max_keys_rejects_new_keys_when_saturated() {
    let mut zone = rate_zone_cfg(1000, 1000, KeyType::Ip);
    zone.max_keys = 2;
    let cfg = cfg_with_rate("ip", zone);
    let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let req_from = |ip: &str| {
        let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            format!("{ip}:1000").parse::<SocketAddr>().unwrap(),
        ));
        req
    };

    // Two distinct client IPs fill the admission cap.
    for ip in ["10.0.0.1", "10.0.0.2"] {
        let resp = app.clone().oneshot(req_from(ip)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // A third, never-seen IP is refused admission outright.
    let third = app.clone().oneshot(req_from("10.0.0.3")).await.unwrap();
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        third
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some(KEY_PRUNE_INTERVAL.as_secs().to_string().as_str())
    );

    // Already-admitted keys keep flowing while the zone is saturated.
    let again = app.oneshot(req_from("10.0.0.1")).await.unwrap();
    assert_eq!(again.status(), StatusCode::OK);
}

#[tokio::test]
async fn inflight_rejection_sets_retry_after() {
    let mut cfg = ApiGatewayConfig::default();
    // in_flight_limit = 0 with no backlog => first request is rejected.
    cfg.in_flight_limit_zones
        .insert("ifl".to_owned(), inflight_zone_cfg(0, KeyType::Ip, vec![]));
    let specs = vec![op(Method::GET, "/x", Some(thr("", "ifl", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("retry-after present");
    assert_eq!(retry, DEFAULT_IN_FLIGHT_RETRY_AFTER_SECS);
}

#[tokio::test]
async fn inflight_excluded_key_bypasses_limit() {
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert(
        "ifl".to_owned(),
        inflight_zone_cfg(1, KeyType::Ip, vec!["unknown".to_owned()]),
    );
    let specs = vec![op(Method::GET, "/x", Some(thr("", "ifl", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    // Client IP resolves to "unknown" (no ConnectInfo/headers), which is excluded.
    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_headers_on_success_response() {
    let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
    let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Metadata headers are exposed on the served response, not the request.
    let headers = resp.headers();
    assert!(headers.contains_key("RateLimit-Policy"));
    assert!(headers.contains_key("RateLimit-Limit"));
    assert!(headers.contains_key("RateLimit-Remaining"));
    assert!(headers.contains_key("X-RateLimit-Limit"));
    assert!(headers.contains_key("X-RateLimit-Remaining"));
}

#[tokio::test]
async fn dry_run_rate_limit_serves_over_burst() {
    // rps 1 / burst 1: the second request would normally be rejected (429),
    // but dry-run serves it and logs instead.
    let cfg = cfg_with_rate("ip", rate_zone_cfg(1, 1, KeyType::Ip));
    let specs = vec![op(Method::GET, "/x", Some(thr_dry("ip", "")))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let first = app
        .clone()
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    // Would-be-throttled request is served instead of rejected.
    let second = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    // Bypassed requests carry no rejection hint.
    assert!(!second.headers().contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn dry_run_does_not_grow_limiter_past_max_keys() {
    // max_keys = 1: in dry-run every request is served, but unadmitted
    // keys must not create limiter state — the keyed store stays capped
    // even in observe mode.
    let mut zone = rate_zone_cfg(1000, 1000, KeyType::Ip);
    zone.max_keys = 1;
    let cfg = cfg_with_rate("ip", zone);
    let specs = vec![op(Method::GET, "/x", Some(thr_dry("ip", "")))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let rate_zone = Arc::clone(
        map.inner
            .routes
            .values()
            .next()
            .unwrap()
            .rate_zone
            .as_ref()
            .unwrap(),
    );

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    for ip in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
        let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            format!("{ip}:1000").parse::<SocketAddr>().unwrap(),
        ));
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    assert!(rate_zone.limiter.len() <= 1);
}

#[tokio::test]
async fn dry_run_in_flight_serves_over_limit() {
    // in_flight_limit = 0 with no backlog => the request would normally be
    // rejected (429), but dry-run serves it and logs instead.
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones
        .insert("ifl".to_owned(), inflight_zone_cfg(0, KeyType::Ip, vec![]));
    let specs = vec![op(Method::GET, "/x", Some(thr_dry("", "ifl")))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!resp.headers().contains_key(header::RETRY_AFTER));
}

#[test]
fn throttle_key_pruner_without_zones_spawns_nothing() {
    // No throttling zones => empty pruner => nothing to spawn (no runtime needed).
    let (_, _, pruner) = build_maps(&[], &ApiGatewayConfig::default()).unwrap();
    assert!(pruner.spawn(CancellationToken::new()).is_none());
}

#[tokio::test]
async fn throttle_key_pruner_task_stops_on_cancel() {
    // A configured zone yields a pruner that spawns a task bound to the
    // lifecycle token; cancelling it must let the task exit cleanly.
    let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
    let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false)))];
    let (_, _, pruner) = build_maps(&specs, &cfg).unwrap();
    let cancel = CancellationToken::new();
    let handle = pruner
        .spawn(cancel.clone())
        .expect("zone present -> prune task spawned");
    cancel.cancel();
    handle.await.expect("prune task joins without panicking");
}

/// Concurrent admits against a small cap: the check-then-insert may overshoot
/// by at most the number of concurrent admitters, and a reset resynchronises
/// the counter with the (now empty) set.
#[test]
fn rate_zone_admit_bounded_overshoot_under_concurrency() {
    const THREADS: u64 = 8;
    const KEYS_PER_THREAD: u64 = 50;
    const CAP: u64 = 10;
    let mut zones = HashMap::new();
    let mut cfg = rate_zone_cfg(1000, 1000, KeyType::Ip);
    cfg.max_keys = CAP;
    let zone = get_or_build_rate_zone(&mut zones, "z", &cfg).unwrap();

    std::thread::scope(|s| {
        for t in 0..THREADS {
            let zone = Arc::clone(&zone);
            s.spawn(move || {
                for k in 0..KEYS_PER_THREAD {
                    let _ = zone.admit(&format!("{t}-{k}"));
                }
            });
        }
    });

    let len = zone.admitted.len() as u64;
    let counted = zone.admitted_len.load(Ordering::Relaxed);
    assert!(len >= CAP, "the cap must be reachable, got {len}");
    assert!(len <= CAP + THREADS, "overshoot must be bounded, got {len}");
    assert_eq!(counted, len, "counter must match the set after all admits");

    zone.reset_admitted();
    assert_eq!(zone.admitted.len(), 0);
    assert_eq!(zone.admitted_len.load(Ordering::Relaxed), 0);
}

/// Same property for in-flight gates: bounded overshoot, and the sweep
/// resynchronises the counter with the map.
#[test]
fn inflight_gate_bounded_overshoot_under_concurrency() {
    const THREADS: u64 = 8;
    const KEYS_PER_THREAD: u64 = 50;
    const CAP: u64 = 10;
    let zone = Arc::new(inflight_zone(CAP));

    std::thread::scope(|s| {
        for t in 0..THREADS {
            let zone = Arc::clone(&zone);
            s.spawn(move || {
                for k in 0..KEYS_PER_THREAD {
                    drop(zone.gate(&format!("{t}-{k}")));
                }
            });
        }
    });

    let len = zone.keys.len() as u64;
    let counted = zone.tracked.load(Ordering::Relaxed);
    assert!(len >= CAP, "the cap must be reachable, got {len}");
    assert!(len <= CAP + THREADS, "overshoot must be bounded, got {len}");
    assert_eq!(counted, len, "counter must match the map after all inserts");

    zone.prune_idle_keys(); // every gate is idle: all dropped
    assert_eq!(zone.keys.len(), 0);
    assert_eq!(zone.tracked.load(Ordering::Relaxed), 0);
}

fn inflight_zone(max_keys: u64) -> InFlightZone {
    let mut cfg = inflight_zone_cfg(1, KeyType::Ip, vec![]);
    cfg.max_keys = max_keys;
    InFlightZone {
        name: "test".to_owned(),
        cfg,
        keys: DashMap::new(),
        tracked: AtomicU64::new(0),
        excluded: HashSet::new(),
    }
}

#[test]
fn inflight_gate_caps_new_keys_until_prune() {
    // The hot path never scans/evicts: past `max_keys` a never-seen key is
    // refused (None) instead of inserted, until the sweep frees capacity.
    let zone = inflight_zone(1);
    assert!(zone.gate("a").is_some());
    assert!(zone.gate("a").is_some(), "known key passes at the cap");
    assert!(zone.gate("b").is_none(), "new key refused at the cap");
    assert_eq!(zone.keys.len(), 1);
    assert_eq!(zone.tracked.load(Ordering::Relaxed), 1);

    zone.prune_idle_keys(); // "a" is idle (map-only reference): dropped
    assert_eq!(zone.tracked.load(Ordering::Relaxed), 0);
    assert!(
        zone.gate("b").is_some(),
        "admission reopens after the sweep"
    );
}

#[test]
fn inflight_prune_idle_keys_drops_only_unreferenced() {
    // At the cap, the sweep drops gates with no in-flight holder
    // (strong_count == 1) and keeps those still referenced by a request.
    let zone = inflight_zone(2);
    let held = zone.gate("held").unwrap(); // strong_count 2 (map + this handle)
    drop(zone.gate("idle").unwrap()); // strong_count 1 (map only)
    assert_eq!(zone.keys.len(), 2);

    zone.prune_idle_keys();

    assert!(zone.keys.contains_key("held"));
    assert!(!zone.keys.contains_key("idle"));
    assert_eq!(zone.tracked.load(Ordering::Relaxed), 1);
    assert_eq!(zone.keys.len(), 1);
    drop(held);
}

#[test]
fn inflight_prune_idle_keys_skips_scan_under_cap() {
    // Under the cap the scan is skipped, so idle gates are retained.
    let zone = inflight_zone(100);
    drop(zone.gate("idle").unwrap());
    zone.prune_idle_keys();
    assert!(zone.keys.contains_key("idle"));
}

#[tokio::test]
async fn in_flight_max_keys_rejects_new_keys_when_saturated() {
    let mut zone = inflight_zone_cfg(4, KeyType::Ip, vec![]);
    zone.max_keys = 2;
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert("ifl".to_owned(), zone);
    let specs = vec![op(Method::GET, "/x", Some(thr("", "ifl", false)))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    let req_from = |ip: &str| {
        let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            format!("{ip}:1000").parse::<SocketAddr>().unwrap(),
        ));
        req
    };

    // Two distinct client IPs fill the cap.
    for ip in ["10.0.0.1", "10.0.0.2"] {
        let resp = app.clone().oneshot(req_from(ip)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // A third, never-seen IP is refused before a gate is created.
    let third = app.clone().oneshot(req_from("10.0.0.3")).await.unwrap();
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        third
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some(KEY_PRUNE_INTERVAL.as_secs().to_string().as_str())
    );

    // Known keys keep flowing while the zone is saturated.
    let again = app.oneshot(req_from("10.0.0.1")).await.unwrap();
    assert_eq!(again.status(), StatusCode::OK);
}

#[tokio::test]
async fn dry_run_in_flight_does_not_grow_gates_past_max_keys() {
    // max_keys = 1: in dry-run every request is served, but unadmitted keys
    // never create a gate, so the map stays capped in observe mode too.
    let mut zone = inflight_zone_cfg(4, KeyType::Ip, vec![]);
    zone.max_keys = 1;
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert("ifl".to_owned(), zone);
    let specs = vec![op(Method::GET, "/x", Some(thr_dry("", "ifl")))];
    let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
    let inflight_zone = Arc::clone(
        map.inner.routes[&(Method::GET, "/x".to_owned())]
            .inflight_zone
            .as_ref()
            .unwrap(),
    );

    let app = Router::new()
        .route("/x", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_no_auth_middleware(map, req, next).await }
            },
        ));

    for ip in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
        let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            format!("{ip}:1000").parse::<SocketAddr>().unwrap(),
        ));
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "dry-run always serves");
    }

    assert_eq!(inflight_zone.keys.len(), 1);
    assert_eq!(inflight_zone.tracked.load(Ordering::Relaxed), 1);
}

fn k8s(namespace: &str, service_account: &str, pod: Option<&str>) -> PlatformIdentity {
    PlatformIdentity::KubernetesServiceAccount {
        namespace: namespace.to_owned(),
        service_account: service_account.to_owned(),
        pod: pod.map(str::to_owned),
    }
}

fn spiffe(trust_domain: &str, name: &str, version: &str) -> PlatformIdentity {
    PlatformIdentity::Spiffe {
        trust_domain: trust_domain.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
    }
}

fn tenant_context() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(42))
        .subject_tenant_id(uuid::Uuid::from_u128(1))
        .build()
        .unwrap()
}

fn platform_op(throttling: ThrottlingSpec) -> OperationSpec {
    let mut spec = op(Method::GET, "/p", Some(throttling));
    spec.auth = RouteAuth::Platform;
    spec
}

/// Serve `/p` behind the post-auth throttling map built from `cfg`.
fn platform_app(spec: OperationSpec, cfg: &ApiGatewayConfig) -> Router {
    let map = ThrottlingMap::from_specs(&[spec], cfg).unwrap();
    Router::new()
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_middleware(map, req, next).await }
            },
        ))
}

fn platform_request(identity: Option<PlatformIdentity>) -> Request {
    let mut req = Request::builder().uri("/p").body(Body::empty()).unwrap();
    if let Some(identity) = identity {
        req.extensions_mut()
            .insert(PlatformSecurityContext::new(identity));
    }
    req
}

#[test]
fn platform_key_names_the_workload_not_the_instance() {
    let key = platform_key(&k8s("ns", "sa", Some("pod-1")));
    assert_eq!(key.as_deref(), Some("platform:k8s:ns/sa"));
    assert_eq!(platform_key(&k8s("ns", "sa", Some("pod-2"))), key);
    assert_eq!(platform_key(&k8s("ns", "sa", None)), key);
    assert_ne!(platform_key(&k8s("other", "sa", None)), key);
    assert_ne!(platform_key(&k8s("ns", "other", None)), key);

    let key = platform_key(&spiffe("example.org", "gear", "1.0.0"));
    assert_eq!(key.as_deref(), Some("platform:spiffe:example.org/gear"));
    assert_eq!(platform_key(&spiffe("example.org", "gear", "1.1.0")), key);
    assert_ne!(platform_key(&spiffe("other.org", "gear", "1.0.0")), key);

    assert_eq!(
        platform_key(&PlatformIdentity::Shared {
            name: "worker".to_owned()
        })
        .as_deref(),
        Some("platform:shared:worker")
    );
}

#[test]
fn platform_key_encodes_components_unambiguously() {
    assert_eq!(
        platform_key(&k8s("a", "b/c", None)).as_deref(),
        Some("platform:k8s:a/b%2Fc")
    );
    assert_eq!(
        platform_key(&k8s("a/b", "c", None)).as_deref(),
        Some("platform:k8s:a%2Fb/c")
    );
    // `%` is escaped first, so a literal `%2F` cannot pass for an encoded `/`.
    assert_ne!(
        platform_key(&k8s("a%2Fb", "c", None)),
        platform_key(&k8s("a/b", "c", None))
    );
    assert_eq!(
        platform_key(&PlatformIdentity::Shared {
            name: "x/y%z".to_owned()
        })
        .as_deref(),
        Some("platform:shared:x%2Fy%25z")
    );
}

#[test]
fn platform_key_refuses_identities_that_name_no_caller() {
    assert_eq!(platform_key(&PlatformIdentity::OutboundMarker), None);
    assert_eq!(platform_key(&PlatformIdentity::Unknown), None);
}

#[test]
fn compute_key_reads_only_the_operation_plane() {
    let identity = |auth| move |req: &Request| compute_key(KeyType::Identity, auth, req, 0);

    // Platform route: only the platform context counts.
    let mut req = platform_request(Some(k8s("ns", "sa", None)));
    assert_eq!(
        identity(RouteAuth::Platform)(&req).as_deref(),
        Some("platform:k8s:ns/sa")
    );
    req.extensions_mut().insert(tenant_context());
    assert_eq!(
        identity(RouteAuth::Platform)(&req).as_deref(),
        Some("platform:k8s:ns/sa")
    );
    let mut tenant_only = platform_request(None);
    tenant_only.extensions_mut().insert(tenant_context());
    assert_eq!(identity(RouteAuth::Platform)(&tenant_only), None);
    assert_eq!(
        identity(RouteAuth::Platform)(&platform_request(Some(PlatformIdentity::OutboundMarker))),
        None
    );

    // Tenant route: a platform context never stands in for the subject.
    assert_eq!(
        identity(RouteAuth::Authenticated)(&platform_request(Some(k8s("ns", "sa", None)))),
        None
    );
    assert_eq!(
        identity(RouteAuth::Authenticated)(&req).as_deref(),
        Some("00000000-0000-0000-0000-00000000002a")
    );

    // Anonymous: rejected at startup, and never keyed on whatever context is present.
    assert_eq!(identity(RouteAuth::Anonymous)(&req), None);
}

#[test]
fn identity_zones_build_on_platform_operations_even_with_auth_disabled() {
    let mut cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    cfg.in_flight_limit_zones.insert(
        "ifl".to_owned(),
        inflight_zone_cfg(1, KeyType::Identity, vec![]),
    );
    let platform = platform_op(thr("id", "ifl", true));
    assert!(ThrottlingMap::from_specs(std::slice::from_ref(&platform), &cfg).is_ok());

    // The platform plane still validates its caller with auth disabled; only
    // the tenant identity collapses to the default subject.
    cfg.auth_disabled = true;
    assert!(ThrottlingMap::from_specs(&[platform], &cfg).is_ok());
    let tenant = op(Method::GET, "/t", Some(thr("id", "ifl", true)));
    let err = ThrottlingMap::from_specs(&[tenant], &cfg)
        .err()
        .expect("tenant identity zone should error")
        .to_string();
    assert!(err.contains("auth_disabled=true"), "{err}");
}

#[test]
fn identity_zone_may_be_shared_by_tenant_and_platform_operations() {
    let cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    let specs = vec![
        op(Method::GET, "/t", Some(thr("id", "", true))),
        platform_op(thr("id", "", true)),
    ];
    let (map, _, _) = build_maps(&specs, &cfg).unwrap();
    let tenant = map
        .inner
        .routes
        .get(&(Method::GET, "/t".to_owned()))
        .unwrap();
    let platform = map
        .inner
        .routes
        .get(&(Method::GET, "/p".to_owned()))
        .unwrap();
    assert_eq!(tenant.auth, RouteAuth::Authenticated);
    assert_eq!(platform.auth, RouteAuth::Platform);
    assert!(Arc::ptr_eq(
        tenant.rate_zone.as_ref().unwrap(),
        platform.rate_zone.as_ref().unwrap()
    ));
}

#[tokio::test]
async fn platform_rate_limit_is_shared_by_workload_replicas() {
    let cfg = cfg_with_rate("id", rate_zone_cfg(1, 1, KeyType::Identity));
    let app = platform_app(platform_op(thr("id", "", true)), &cfg);

    let send = |identity| app.clone().oneshot(platform_request(Some(identity)));
    assert_eq!(
        send(k8s("ns", "sa", Some("pod-1"))).await.unwrap().status(),
        StatusCode::OK
    );
    let replica = send(k8s("ns", "sa", Some("pod-2"))).await.unwrap();
    assert_eq!(replica.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(replica.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(
        send(k8s("ns", "other", Some("pod-1")))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn platform_inflight_excluded_key_bypasses_limit() {
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert(
        "ifl".to_owned(),
        inflight_zone_cfg(1, KeyType::Identity, vec!["platform:k8s:ns/sa".to_owned()]),
    );
    let map = ThrottlingMap::from_specs(&[platform_op(thr("", "ifl", true))], &cfg).unwrap();
    let zone = Arc::clone(
        map.inner.routes[&(Method::GET, "/p".to_owned())]
            .inflight_zone
            .as_ref()
            .unwrap(),
    );
    let app = Router::new()
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_middleware(map, req, next).await }
            },
        ));

    let resp = app
        .clone()
        .oneshot(platform_request(Some(k8s("ns", "sa", Some("pod-1")))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(zone.keys.len(), 0, "an excluded key never gets a gate");

    // Hold the only slot of a non-excluded workload: its next request is refused.
    let _held = zone
        .gate("platform:k8s:ns/other")
        .unwrap()
        .try_acquire()
        .unwrap();
    let resp = app
        .oneshot(platform_request(Some(k8s("ns", "other", Some("pod-1")))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn tenant_inflight_excluded_raw_uuid_still_bypasses_limit() {
    let subject = tenant_context().subject_id().to_string();
    let mut cfg = ApiGatewayConfig::default();
    cfg.in_flight_limit_zones.insert(
        "ifl".to_owned(),
        inflight_zone_cfg(1, KeyType::Identity, vec![subject.clone()]),
    );
    let map = ThrottlingMap::from_specs(&[op(Method::GET, "/t", Some(thr("", "ifl", true)))], &cfg)
        .unwrap();
    let zone = Arc::clone(
        map.inner.routes[&(Method::GET, "/t".to_owned())]
            .inflight_zone
            .as_ref()
            .unwrap(),
    );
    // Even a held slot under the subject's key cannot refuse an excluded key.
    let _held = zone.gate(&subject).unwrap().try_acquire().unwrap();
    let app = Router::new()
        .route("/t", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_middleware(map, req, next).await }
            },
        ));
    let mut req = Request::builder().uri("/t").body(Body::empty()).unwrap();
    req.extensions_mut().insert(tenant_context());
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn shared_zone_keeps_tenant_and_platform_buckets_apart() {
    // A platform caller named like a tenant subject still gets its own bucket.
    let cfg = cfg_with_rate("id", rate_zone_cfg(1, 1, KeyType::Identity));
    let specs = vec![
        op(Method::GET, "/t", Some(thr("id", "", true))),
        platform_op(thr("id", "", true)),
    ];
    let (map, _, _) = build_maps(&specs, &cfg).unwrap();
    let app = Router::new()
        .route("/t", get(|| async { "ok" }))
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            move |req: Request, next: Next| {
                let map = map.clone();
                async move { throttling_middleware(map, req, next).await }
            },
        ));
    let tenant = || {
        let mut req = Request::builder().uri("/t").body(Body::empty()).unwrap();
        req.extensions_mut().insert(tenant_context());
        req
    };
    let impostor = || {
        platform_request(Some(PlatformIdentity::Shared {
            name: tenant_context().subject_id().to_string(),
        }))
    };

    assert_eq!(
        app.clone().oneshot(tenant()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone().oneshot(impostor()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone().oneshot(tenant()).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        app.oneshot(impostor()).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn dry_run_platform_rate_limit_serves_over_burst() {
    let cfg = cfg_with_rate("id", rate_zone_cfg(1, 1, KeyType::Identity));
    let mut spec = thr("id", "", true);
    spec.dry_run = true;
    let app = platform_app(platform_op(spec), &cfg);

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(platform_request(Some(k8s("ns", "sa", None))))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "dry-run always serves");
    }
}

#[tokio::test]
async fn platform_zone_without_keyable_identity_is_internal_error_even_in_dry_run() {
    let mut cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
    cfg.in_flight_limit_zones.insert(
        "ifl".to_owned(),
        inflight_zone_cfg(1, KeyType::Identity, vec![]),
    );
    for (rate, inflight) in [("id", ""), ("", "ifl")] {
        for dry_run in [false, true] {
            let mut spec = thr(rate, inflight, true);
            spec.dry_run = dry_run;
            let app = platform_app(platform_op(spec), &cfg);

            let mut tenant_only = platform_request(None);
            tenant_only.extensions_mut().insert(tenant_context());
            for req in [
                platform_request(None),
                tenant_only,
                platform_request(Some(PlatformIdentity::OutboundMarker)),
                platform_request(Some(PlatformIdentity::Unknown)),
            ] {
                let resp = app.clone().oneshot(req).await.unwrap();
                assert_eq!(
                    resp.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "rate={rate:?} inflight={inflight:?} dry_run={dry_run}"
                );
            }
        }
    }
}

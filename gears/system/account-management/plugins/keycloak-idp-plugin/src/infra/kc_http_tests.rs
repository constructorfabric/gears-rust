use super::*;
use crate::config::{BackoffKind, HttpRetryPolicy, JitterKind};
use crate::domain::kc::transport::{KcTransport, truncate_2kb};

// Test-only constructors. Production code reaches the transport through
// `ReqwestKcTransport::from_config`, which plumbs the retry policy from
// `KeycloakConfig`; these short-form builders only matter when wiremock
// + a `reqwest::Client::new()` are enough and the test wants to dictate
// (or default) the retry policy explicitly. Lives in the companion test
// file rather than next to `from_config` so DE1101 stays happy (no
// `#[cfg(test)] fn` items in a file with a companion `_tests.rs`).
impl ReqwestKcTransport {
    #[must_use]
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            retry_policy: HttpRetryPolicy::default(),
        }
    }

    #[must_use]
    pub(crate) fn with_retry_policy(http: reqwest::Client, retry_policy: HttpRetryPolicy) -> Self {
        Self { http, retry_policy }
    }
}

/// Verify that every outbound Keycloak Admin REST call carries a W3C
/// `traceparent` header extracted from the current tracing span.
///
/// Matches DESIGN §8.2: "Every Keycloak Admin REST call carries a W3C
/// `traceparent` (and `tracestate` when present) header extracted from the
/// current tracing span via `tracing-opentelemetry`, so the Keycloak hop
/// joins the platform trace tree."
#[tokio::test]
async fn raw_request_injects_traceparent_header() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    // 1. Stand up a wiremock server that captures the incoming headers.
    //    We store `Option<bool>` to record whether `traceparent` was present;
    //    this avoids a direct dependency on the `http` crate in test code.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<bool>));
    let server = wiremock::MockServer::start().await;
    let cap = captured.clone();
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(move |req: &wiremock::Request| {
            let has_tp = req.headers.contains_key("traceparent");
            *cap.lock().unwrap() = Some(has_tp);
            wiremock::ResponseTemplate::new(200).set_body_string("{}")
        })
        .mount(&server)
        .await;

    // 2. Install a tracing subscriber with an OTel layer so that spans
    //    emit OTel context. `set_default` returns a guard that restores the
    //    previous default when dropped — safe to use in parallel tests.
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("keycloak-idp-plugin-test");
    let subscriber = tracing_subscriber::registry().with(OpenTelemetryLayer::new(tracer));
    let _guard = subscriber.set_default();

    // 3. Enter a span and make the outbound request — inject_trace_headers
    //    should populate traceparent from this span's OTel context.
    let span = tracing::info_span!("test-traceparent-span");
    let _enter = span.enter();

    let transport = ReqwestKcTransport::new(reqwest::Client::new());
    let url = format!("{}/admin/realms/test", server.uri());
    let _ = transport
        .raw_request("GET", &url, "dummy-token", None, "/admin/realms/{realm}")
        .await;

    // 4. Assert the header was injected.
    let guard = captured.lock().unwrap();
    let has_traceparent = guard.expect("wiremock must have captured a request");
    assert!(
        has_traceparent,
        "traceparent header must be injected on outbound KC requests"
    );
}

#[test]
fn truncate_short_passthrough() {
    let s = "hello world";
    assert_eq!(truncate_2kb(s), "hello world");
}

#[test]
fn truncate_long_appends_ellipsis() {
    let s: String = "a".repeat(3000);
    let out = truncate_2kb(&s);
    assert!(out.ends_with('…'));
    // 2048 'a' chars + ellipsis = 2049 chars
    assert_eq!(out.chars().count(), 2049);
}

#[test]
fn truncate_escapes_non_printable_ascii() {
    // \x01 is non-printable
    let s = "ok\x01here";
    let out = truncate_2kb(s);
    assert_eq!(out, "ok\\x01here");
}

#[test]
fn method_static_covers_canonical_verbs() {
    assert_eq!(method_static(&Method::GET), "GET");
    assert_eq!(method_static(&Method::POST), "POST");
    assert_eq!(method_static(&Method::PUT), "PUT");
    assert_eq!(method_static(&Method::DELETE), "DELETE");
}

#[test]
fn http_status_error_constructs_with_truncated_body() {
    let err = http_status_error(&Method::GET, "/admin/realms/{realm}", 404, "not found");
    assert_eq!(
        format!("{err}"),
        "kc:GET /admin/realms/{realm} -> 404: not found"
    );
}

#[test]
#[allow(clippy::panic)] // test helper: intentional panic on unexpected variant
fn http_status_error_redacts_client_secret_in_body_capture() {
    let body = r#"{"error":"bad","client_secret":"leak-value"}"#;
    let err = http_status_error(&Method::GET, "/admin/realms/{realm}", 400, body);
    match err {
        PluginError::KcRest { body_first_2kb, .. } => {
            assert!(
                !body_first_2kb.contains("leak-value"),
                "secret must be redacted: {body_first_2kb}"
            );
            assert!(
                body_first_2kb.contains("<redacted>"),
                "redaction marker absent: {body_first_2kb}"
            );
        }
        other => panic!("expected KcRest, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Retry-policy tests (DESIGN §4.4 / §10)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a `ReqwestKcTransport` with zero-delay retry policy (deterministic,
/// fast tests) and `max_retries = n`.
fn transport_with_retries(max_retries: u32) -> ReqwestKcTransport {
    let policy = HttpRetryPolicy {
        max_retries,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 0,
        max_backoff_ms: 0,
    };
    ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy)
}

#[tokio::test]
async fn raw_request_retries_on_502_then_succeeds() {
    // Wire: 502 twice, then 200.
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(502).set_body_string("bad gateway"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let transport = transport_with_retries(3);
    let url = format!("{}/admin/realms/test", server.uri());
    let (status, body) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must succeed after retries");

    assert_eq!(status, 200);
    assert_eq!(body, "ok");
    // 3 total requests: 2 failures + 1 success.
    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(reqs.len(), 3);
}

#[tokio::test]
async fn raw_request_exhausts_retries_on_persistent_503() {
    // Wire: always 503.
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(503).set_body_string("unavailable"))
        .mount(&server)
        .await;

    let transport = transport_with_retries(2);
    let url = format!("{}/admin/realms/test", server.uri());
    let (status, _body) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("should return last status, not error");

    // Returns 503 to the caller after exhausting retries.
    assert_eq!(status, 503);
    // max_retries=2 → 3 total requests (1 original + 2 retries).
    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(reqs.len(), 3);
}

#[tokio::test]
async fn raw_request_retries_on_429_then_succeeds() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(429).set_body_string("rate limited"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let transport = transport_with_retries(3);
    let url = format!("{}/admin/realms/test", server.uri());
    let (status, body) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must succeed after retry");

    assert_eq!(status, 200);
    assert_eq!(body, "ok");
    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(reqs.len(), 2);
}

/// RFC 9110 §10.2.3 — when the server returns `Retry-After: <seconds>` on
/// a 429 or 503, the transport MUST honour the header instead of falling
/// straight through to the computed exponential backoff. Verified by
/// configuring zero-delay backoff and asserting elapsed time ≥ header.
#[tokio::test]
async fn raw_request_honours_retry_after_header_on_429() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(429)
                .insert_header("Retry-After", "1")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    // initial=0 / max=10000: without honouring Retry-After the retry
    // would fire essentially instantly. Honouring the header forces a
    // ~1s wait before the second attempt lands.
    let policy = HttpRetryPolicy {
        max_retries: 3,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 0,
        max_backoff_ms: 10_000,
    };
    let transport = ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy);
    let url = format!("{}/admin/realms/test", server.uri());
    let started = std::time::Instant::now();
    let (status, _) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must succeed after Retry-After-driven retry");
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "Retry-After: 1 must drive at least ~1s of delay; elapsed = {elapsed:?}",
    );
}

/// A hostile or misconfigured server returning `Retry-After: 3600` (1
/// hour) MUST NOT pin a worker thread for that long. The transport
/// caps the parsed value at `retry_policy.max_backoff_ms` before
/// sleeping.
#[tokio::test]
async fn raw_request_caps_retry_after_at_max_backoff_ms() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(429)
                .insert_header("Retry-After", "3600")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let policy = HttpRetryPolicy {
        max_retries: 1,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 0,
        // 100ms cap — the 3600s header value must collapse to this.
        max_backoff_ms: 100,
    };
    let transport = ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy);
    let url = format!("{}/admin/realms/test", server.uri());
    let started = std::time::Instant::now();
    let (status, _) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must complete fast despite hostile Retry-After");
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "Retry-After must be capped by max_backoff_ms; elapsed = {elapsed:?}",
    );
}

#[tokio::test]
async fn raw_request_no_retry_on_400() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("bad request"))
        .mount(&server)
        .await;

    let transport = transport_with_retries(3);
    let url = format!("{}/admin/realms/test", server.uri());
    let (status, _body) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must return status without retry");

    assert_eq!(status, 400);
    // Exactly 1 request — no retry on 4xx (except 429).
    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(reqs.len(), 1);
}

#[tokio::test]
async fn raw_request_no_retry_on_404() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let transport = transport_with_retries(3);
    let url = format!("{}/admin/realms/test", server.uri());
    let (status, _body) = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await
        .expect("must return status without retry");

    assert_eq!(status, 404);
    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(reqs.len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Method-aware transport retry (create-path idempotency)
//
// A connect/timeout error does NOT prove the server didn't process the
// request. Retrying a NON-idempotent POST on such an error can therefore
// duplicate a side effect — exactly the double `POST /admin/realms` that
// created a duplicate realm and produced the false-ambiguous 500. So the
// transport retries connect/timeout ONLY for idempotent verbs (GET/PUT/
// DELETE) and for the token-endpoint `form_post` (re-fetching a token is
// safe). A non-idempotent `raw_request` POST must be surfaced, not replayed.
// ─────────────────────────────────────────────────────────────────────────────

/// Zero-backoff retry policy + a client with a short request timeout, so a
/// `set_delay` longer than the timeout deterministically produces a
/// `reqwest` timeout (transport) error fast.
fn transport_short_timeout(max_retries: u32) -> ReqwestKcTransport {
    let policy = HttpRetryPolicy {
        max_retries,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 0,
        max_backoff_ms: 0,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(150))
        .build()
        .expect("client builds");
    ReqwestKcTransport::with_retry_policy(client, policy)
}

/// A non-idempotent `POST` that TIMES OUT must be surfaced after exactly ONE
/// attempt — never replayed (the first POST may have already taken effect).
#[tokio::test]
async fn raw_request_does_not_retry_post_on_timeout() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            wiremock::ResponseTemplate::new(201).set_delay(std::time::Duration::from_millis(800)),
        )
        .mount(&server)
        .await;

    let transport = transport_short_timeout(3);
    let url = format!("{}/admin/realms", server.uri());
    let err = transport
        .raw_request("POST", &url, "tok", None, "/admin/realms")
        .await
        .expect_err("timed-out POST must surface, not silently succeed");
    assert!(
        matches!(
            err,
            PluginError::KcRest {
                status: crate::domain::error::KcStatusKind::Timeout,
                ..
            }
        ),
        "expected a Timeout KcRest, got {err:?}",
    );

    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert_eq!(
        reqs.len(),
        1,
        "non-idempotent POST must NOT be retried on timeout; observed {} attempts",
        reqs.len(),
    );
}

/// An idempotent `GET` that times out IS retried on transport failure
/// (re-reading is harmless) — guards that the method-aware gate doesn't
/// over-restrict.
#[tokio::test]
async fn raw_request_retries_get_on_timeout() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(800)),
        )
        .mount(&server)
        .await;

    let transport = transport_short_timeout(2);
    let url = format!("{}/admin/realms/test", server.uri());
    let _ = transport
        .raw_request("GET", &url, "tok", None, "/admin/realms/{realm}")
        .await;

    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert!(
        reqs.len() >= 2,
        "idempotent GET must retry on timeout; observed {} attempt(s)",
        reqs.len(),
    );
}

/// The token-endpoint `form_post` is a POST, but re-fetching a token is
/// safe and important under load — it MUST still retry on timeout (this is
/// the path that would otherwise be starved if we naively gated all POSTs).
#[tokio::test]
async fn form_post_retries_on_timeout() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(800)),
        )
        .mount(&server)
        .await;

    let transport = transport_short_timeout(2);
    let url = format!(
        "{}/realms/master/protocol/openid-connect/token",
        server.uri()
    );
    let _ = transport
        .form_post(
            &url,
            &[("grant_type", "client_credentials")],
            "/realms/{realm}/protocol/openid-connect/token",
        )
        .await;

    let reqs = server
        .received_requests()
        .await
        .expect("must have requests");
    assert!(
        reqs.len() >= 2,
        "token form_post must retry on timeout; observed {} attempt(s)",
        reqs.len(),
    );
}

#[tokio::test]
#[allow(clippy::panic)] // test helper: intentional panic on unexpected variant
async fn raw_request_rejects_unknown_http_method() {
    // A typo in a path-template constant — e.g. "DLETE" — used to silently
    // coerce to GET, turning a delete into a read. The transport must now
    // reject the verb up front.
    let transport = transport_with_retries(0);
    let err = transport
        .raw_request(
            "DLETE",
            "https://kc.invalid/admin/realms/test",
            "tok",
            None,
            "/admin/realms/{realm}",
        )
        .await
        .expect_err("invalid method must fail before any HTTP exchange");
    match err {
        PluginError::Config { detail } => {
            assert!(
                detail.contains("DLETE"),
                "detail should name the offending verb: {detail}"
            );
        }
        other => panic!("expected PluginError::Config, got {other:?}"),
    }
}

#[test]
fn backoff_ms_none_jitter_is_deterministic_exponential() {
    let policy = HttpRetryPolicy {
        max_retries: 5,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 100,
        max_backoff_ms: 10_000,
    };
    let transport = ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy);

    // attempt 0 → 100ms, attempt 1 → 200ms, attempt 2 → 400ms, attempt 3 → 800ms
    assert_eq!(transport.backoff_ms(0), 100);
    assert_eq!(transport.backoff_ms(1), 200);
    assert_eq!(transport.backoff_ms(2), 400);
    assert_eq!(transport.backoff_ms(3), 800);
}

#[test]
fn backoff_ms_none_jitter_is_capped_at_max() {
    let policy = HttpRetryPolicy {
        max_retries: 5,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::None,
        initial_backoff_ms: 100,
        max_backoff_ms: 300,
    };
    let transport = ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy);

    // 100, 200, then capped at 300 for any higher attempt
    assert_eq!(transport.backoff_ms(0), 100);
    assert_eq!(transport.backoff_ms(1), 200);
    assert_eq!(transport.backoff_ms(2), 300);
    assert_eq!(transport.backoff_ms(10), 300);
}

#[test]
fn backoff_ms_full_jitter_within_bounds() {
    let policy = HttpRetryPolicy {
        max_retries: 5,
        backoff: BackoffKind::Exponential,
        jitter: JitterKind::Full,
        initial_backoff_ms: 100,
        max_backoff_ms: 5_000,
    };
    let transport = ReqwestKcTransport::with_retry_policy(reqwest::Client::new(), policy);

    // Full jitter → result must be in [0, cap]. Cap at attempt 0 is 100ms.
    for _ in 0..20 {
        let delay = transport.backoff_ms(0);
        assert!(delay <= 100, "full jitter delay {delay} exceeds cap 100");
    }
    // At attempt 3 cap is 800ms.
    for _ in 0..20 {
        let delay = transport.backoff_ms(3);
        assert!(delay <= 800, "full jitter delay {delay} exceeds cap 800");
    }
}

use super::*;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware::from_fn,
    routing::get,
};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use tower::ServiceExt;

fn problem_response(problem: &Problem, status: StatusCode) -> Response {
    let body = serde_json::to_vec(problem).expect("serialize problem");
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, PROBLEM_JSON)
        .body(Body::from(body))
        .expect("build response")
}

fn build_app(responder: impl Fn() -> Response + Clone + Send + Sync + 'static) -> Router {
    Router::new()
        .route(
            "/api/v1/widgets/42",
            get(move || {
                let responder = responder.clone();
                async move { responder() }
            }),
        )
        .layer(from_fn(canonical_error_middleware))
}

async fn body_to_problem(response: Response) -> Problem {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse problem+json")
}

async fn body_to_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("parse problem+json")
}

#[tokio::test]
async fn fills_instance_and_trace_id_from_headers() {
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let problem = body_to_problem(res).await;

    assert_eq!(problem.instance.as_deref(), Some("/api/v1/widgets/42"));
    // Only the 32-hex trace-id segment is surfaced on the wire — matches
    // the format the access log and OTel span recording use, so an
    // operator can grep the same value across all three.
    assert_eq!(
        problem.trace_id.as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );
}

#[tokio::test]
async fn does_not_overwrite_existing_instance() {
    let preset: Problem =
        Problem::from(CanonicalError::internal("boom").create()).with_instance("/handler-set");
    let app = build_app(move || problem_response(&preset, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let problem = body_to_problem(res).await;

    assert_eq!(problem.instance.as_deref(), Some("/handler-set"));
}

#[tokio::test]
async fn does_not_overwrite_existing_trace_id() {
    let preset: Problem =
        Problem::from(CanonicalError::internal("boom").create()).with_trace_id("handler-trace");
    let app = build_app(move || problem_response(&preset, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header("traceparent", "should-be-ignored")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let problem = body_to_problem(res).await;

    assert_eq!(problem.trace_id.as_deref(), Some("handler-trace"));
}

#[tokio::test]
async fn passes_through_non_problem_responses_verbatim() {
    let payload = b"{\"hello\":\"world\"}";
    let app = Router::new()
        .route(
            "/plain",
            get(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(&b"{\"hello\":\"world\"}"[..]))
                    .unwrap()
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/plain")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), payload);
}

#[tokio::test]
async fn foreign_passthrough_marked_response_is_never_touched() {
    // A reverse-proxy layer (`toolkit-gateway::Forwarder`, `oagw`'s
    // proxy data-plane) tags a relayed response `ForeignPassthrough` to
    // say "this is a genuine upstream's own response, hands off." Even
    // a 500 with a non-Problem body and a gear-specific header must
    // survive completely unchanged - no wrap, no enrichment, no read.
    let payload = b"{\"error\":\"upstream exploded\"}";
    let app = Router::new()
        .route(
            "/upload",
            get(|| async {
                let mut response = Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-oagw-error-source", "upstream")
                    .body(Body::from(&b"{\"error\":\"upstream exploded\"}"[..]))
                    .unwrap();
                response.extensions_mut().insert(ForeignPassthrough);
                response
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.headers()
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok()),
        Some("upstream")
    );
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), payload);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn malformed_5xx_problem_is_wrapped_as_internal_with_error_log() {
    // A response claiming `application/problem+json` but not actually
    // valid `Problem` JSON must still become a valid `Problem` - "every
    // error response is a valid RFC 9457 Problem" doesn't have an
    // exception for a response that lied about its own Content-Type.
    // Routed through the same safe fallback as any other untyped
    // rejection (see `enrich_problem_response`), so the malformed body
    // itself is never sent to the client, only logged server-side. A 5xx
    // status here maps to the real `internal` category, not
    // `about:blank` - see `wrap_as_internal_problem`.
    let app = Router::new()
        .route(
            "/api/v1/widgets/42",
            get(|| async {
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, PROBLEM_JSON)
                    .body(Body::from(&b"{not-json}"[..]))
                    .unwrap()
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/api/v1/widgets/42",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
            "context": {},
        })
    );
    assert!(logs_contain(
        "canonical error middleware: failed to deserialize problem+json body"
    ));
}

#[tokio::test]
async fn malformed_body_with_recovered_extension_uses_the_real_category() {
    // The original response WAS correctly built via
    // `CanonicalError::into_response()` - a real `service_unavailable`,
    // not `internal` - so its extensions still carry that real error,
    // even though its body has since been corrupted into invalid JSON
    // (simulating some hypothetical layer that mangles a body but
    // leaves extensions alone). The wrapped Problem must use the
    // recovered `service_unavailable` category and its real detail, not
    // synthesize a generic `internal` from the bare 503 status code.
    use axum::response::IntoResponse;

    let app = Router::new()
        .route(
            "/api/v1/widgets/42",
            get(|| async {
                let mut response = CanonicalError::service_unavailable()
                    .with_detail("authorization evaluation failed")
                    .create()
                    .into_response();
                *response.body_mut() = Body::from(&b"{not-json}"[..]);
                response
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "authorization evaluation failed",
            "instance": "/api/v1/widgets/42",
            "context": {},
        })
    );
}

#[tokio::test]
async fn a_minimal_spec_compliant_foreign_4xx_problem_keeps_its_real_type() {
    // RFC 9457 §3.1: `detail` and `context` are optional members. A
    // genuinely external, spec-compliant Problem (e.g. a well-behaved
    // upstream gear proxied through api-gateway) can omit both and still
    // be fully valid - `Problem`'s `Deserialize` defaults both fields, so
    // this minimal body parses successfully and the real `type`/`title`
    // survive instead of collapsing into a generic `about:blank`.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::CONFLICT,
            PROBLEM_JSON,
            r#"{"type":"https://example.com/probs/out-of-credit","title":"You do not have enough credit.","status":409}"#,
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "https://example.com/probs/out-of-credit",
            "title": "You do not have enough credit.",
            "status": 409,
            "detail": "",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
async fn a_problem_omitting_status_is_normalized_from_the_real_response_status() {
    // RFC 9457 §3.1: `status` is advisory and optional too - a peer that
    // omits it is still fully spec-compliant, and the real response
    // status is right here to fill it in.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::CONFLICT,
            PROBLEM_JSON,
            r#"{"type":"https://example.com/probs/out-of-credit","title":"You do not have enough credit."}"#,
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "https://example.com/probs/out-of-credit",
            "title": "You do not have enough credit.",
            "status": 409,
            "detail": "",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
async fn a_minimal_spec_compliant_foreign_5xx_problem_is_not_relabeled_as_internal() {
    // Same invariant, 5xx side: a genuinely external upstream failure must not
    // be reported to the client as *this platform's own* `internal`
    // category just because its Problem body omitted optional members.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::BAD_GATEWAY,
            PROBLEM_JSON,
            r#"{"type":"https://example.com/probs/upstream-down","title":"Upstream is down.","status":502}"#,
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "https://example.com/probs/upstream-down",
            "title": "Upstream is down.",
            "status": 502,
            "detail": "",
            "instance": "/upload",
            "context": {},
        })
    );
    assert_ne!(
        json["type"], "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        "an external upstream failure must never be relabeled as this platform's own internal error"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn logs_warn_for_4xx_and_error_for_5xx() {
    // 4xx → warn
    let problem_4xx: Problem = CanonicalError::unauthenticated()
        .with_reason("MISSING_TOKEN")
        .create()
        .into();
    let app_4xx = build_app(move || problem_response(&problem_4xx, StatusCode::UNAUTHORIZED));
    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let _ = app_4xx.oneshot(req).await.unwrap();
    assert!(logs_contain("canonical error response (client)"));

    // 5xx → error
    let problem_5xx: Problem = CanonicalError::internal("boom").create().into();
    let app_5xx =
        build_app(move || problem_response(&problem_5xx, StatusCode::INTERNAL_SERVER_ERROR));
    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let _ = app_5xx.oneshot(req).await.unwrap();
    assert!(logs_contain("canonical error response (server)"));
}

#[tokio::test]
async fn log_event_does_not_carry_its_own_trace_id_field() {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt as _;

    // Captures the structured fields of every event, so we can assert on the
    // presence/absence of a field by name rather than by substring.
    #[derive(Clone, Default)]
    struct FieldCapture(Arc<Mutex<Vec<HashMap<String, String>>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FieldCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Visitor<'a>(&'a mut HashMap<String, String>);
            impl tracing::field::Visit for Visitor<'_> {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.insert(f.name().to_owned(), format!("{v:?}"));
                }
                fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                    self.0.insert(f.name().to_owned(), v.to_owned());
                }
                fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
                    self.0.insert(f.name().to_owned(), v.to_string());
                }
                fn record_i64(&mut self, f: &tracing::field::Field, v: i64) {
                    self.0.insert(f.name().to_owned(), v.to_string());
                }
            }
            let mut fields = HashMap::new();
            event.record(&mut Visitor(&mut fields));
            self.0.lock().unwrap().push(fields);
        }
    }

    let capture = FieldCapture::default();
    let events = capture.0.clone();
    let subscriber = tracing_subscriber::registry().with(capture);
    let _guard = tracing::subscriber::set_default(subscriber);

    // A 5xx with a `traceparent` present — so the OLD code path would have put a
    // `trace_id` field on the log event. It must not any more.
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));
    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();

    let captured = events.lock().unwrap().clone();
    let log = captured
        .iter()
        .find(|f| {
            f.get("message")
                .is_some_and(|m| m.contains("canonical error response (server)"))
        })
        .expect("the 5xx path must log a canonical error event");

    // The log event must NOT carry its own `trace_id` field: the log-correlation
    // formatter splices the live span's id onto the top level of the record, so
    // a nested copy here would be a duplicate key.
    assert!(
        !log.contains_key("trace_id"),
        "log_problem must not emit a trace_id field: {log:?}"
    );
    // Sanity: the fields it is supposed to carry are still there.
    assert_eq!(log.get("status").map(String::as_str), Some("500"));
    assert!(log.contains_key("instance"));
    assert!(log.contains_key("problem_type"));
}

#[tokio::test]
async fn extracts_trace_id_from_traceparent_ignoring_other_headers() {
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .header("x-trace-id", "from-x-trace-id")
        .header("x-request-id", "from-x-request-id")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let problem = body_to_problem(res).await;

    // Only the 32-hex trace-id segment of `traceparent` is used. `x-trace-id`
    // and `x-request-id` are present but deliberately ignored — they are not
    // trace ids.
    assert_eq!(
        problem.trace_id.as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );
}

#[tokio::test]
async fn malformed_traceparent_and_no_active_span_yields_no_trace_id() {
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header("traceparent", "not-a-w3c-traceparent")
        .header("x-trace-id", "from-x-trace-id")
        .header("x-request-id", "from-x-request-id")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let problem = body_to_problem(res).await;

    // A malformed traceparent is ignored, and `x-trace-id` / `x-request-id`
    // are deliberately NOT consulted — they are not trace ids. With no active
    // OTel span there is no real trace id to report, so `trace_id` is left
    // absent rather than filled with a non-trace value.
    assert_eq!(problem.trace_id, None);
}

#[cfg(feature = "otel")]
#[tokio::test]
async fn resolves_trace_id_from_active_otel_span_when_no_traceparent() {
    // With no incoming `traceparent`, the middleware reads the live OTel span
    // context (the same source the JSON log-correlation formatter uses), so
    // the wire `trace_id` is a real, correlatable 32-hex id rather than an
    // `x-request-id` or a tracing span handle.
    use opentelemetry::trace::TracerProvider as _;
    use tracing::Instrument as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let tracer = provider.tracer("canonical-error-layer-test");

    // `set_default` returns a guard that restores the previous default when
    // dropped; `#[tokio::test]` runs on a current-thread runtime, so the
    // thread-local default applies across the awaited request future.
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    let _guard = tracing::subscriber::set_default(subscriber);

    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();

    // `.instrument(span)` keeps `span` current while the request future is
    // polled; `OpenTelemetryLayer` attaches the OTel context on span entry,
    // so `Context::current()` inside the middleware carries a valid span.
    let span = tracing::info_span!("otel_trace_id_test");
    let res = app.oneshot(req).instrument(span).await.unwrap();
    let problem = body_to_problem(res).await;

    let trace_id = problem
        .trace_id
        .expect("trace_id should be resolved from the live OTel span");
    assert_eq!(
        trace_id.len(),
        32,
        "trace_id must be 32 hex chars: {trace_id}"
    );
    assert!(
        trace_id.chars().all(|c| c.is_ascii_hexdigit()),
        "trace_id must be lowercase hex: {trace_id}"
    );
    assert_ne!(trace_id, "0".repeat(32), "trace_id must not be the nil id");
}

#[tokio::test]
async fn body_is_valid_json_after_rewrite() {
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).expect("rewritten body must be valid JSON");
    assert_eq!(v["instance"].as_str(), Some("/api/v1/widgets/42"));
    assert_eq!(
        v["trace_id"].as_str(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );
}

fn foreign_response(status: StatusCode, content_type: &str, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("build response")
}

fn build_foreign_app(responder: impl Fn() -> Response + Clone + Send + Sync + 'static) -> Router {
    Router::new()
        .route(
            "/upload",
            get(move || {
                let responder = responder.clone();
                async move { responder() }
            }),
        )
        .layer(from_fn(canonical_error_middleware))
}

#[tokio::test]
async fn wraps_a_foreign_4xx_plain_text_response_as_a_problem() {
    // Simulates a tower-layer short-circuit's own hardcoded response
    // shape (e.g. `tower_http::limit::RequestBodyLimitLayer`), not an
    // actual size-limited request - the fallback doesn't care who
    // produced the response, only that it's non-Problem + error status.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text/plain; charset=utf-8",
            "length limit exceeded",
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(PROBLEM_JSON)
    );
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Payload Too Large",
            "status": 413,
            "detail": "Payload Too Large",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn foreign_body_is_logged_at_debug_but_never_sent_to_client() {
    // The response-body assertion above proves the foreign body's real
    // content ("length limit exceeded") is absent from the client-visible
    // output - that's only half of `wrap_as_generic_problem`'s documented
    // guarantee. This proves the other half: the content is actually
    // captured server-side for diagnosis. Without this, deleting the
    // `tracing::debug!` call inside `log_foreign_body` entirely would
    // leave every other test in this file green while silently losing
    // all operational visibility into what's failing.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text/plain; charset=utf-8",
            "length limit exceeded",
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let json = body_to_json(res).await;
    assert_eq!(json["detail"], "Payload Too Large");
    assert!(logs_contain("length limit exceeded"));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn oversized_foreign_body_still_wraps_correctly() {
    // The foreign body exceeds `MAX_FOREIGN_BODY_LOG_BYTES` - the
    // diagnostic `to_bytes` read fails with a length-limit error, but
    // wrapping must still succeed with the correct status and the
    // reason-phrase `Problem`, exactly as if the body had been empty.
    let big_body = "x".repeat(MAX_FOREIGN_BODY_LOG_BYTES + 1024);
    let app = build_foreign_app(move || {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(big_body.clone()))
            .expect("build response")
    });

    let req = Request::builder()
        .uri("/upload")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Payload Too Large",
            "status": 413,
            "detail": "Payload Too Large",
            "instance": "/upload",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
            "context": {},
        })
    );
    assert!(logs_contain(
        "canonical error middleware: failed to read foreign response body while wrapping"
    ));
}

#[tokio::test]
#[tracing_test::traced_test]
async fn oversized_problem_json_body_is_wrapped_not_left_empty() {
    // A response already labeled `application/problem+json` whose body
    // exceeds `MAX_PROBLEM_BODY_BYTES` must still become a valid `Problem`
    // - the pre-existing behavior here returned an empty body outright,
    // which is itself not a valid Problem and violates the same
    // guarantee this whole module exists to uphold.
    let big_body = "x".repeat(MAX_PROBLEM_BODY_BYTES + 1024);
    let app = build_foreign_app(move || {
        Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, PROBLEM_JSON)
            .body(Body::from(big_body.clone()))
            .expect("build response")
    });

    let req = Request::builder()
        .uri("/upload")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 503,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/upload",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
            "context": {},
        })
    );
    assert!(logs_contain(
        "canonical error middleware: failed to read response body"
    ));
}

#[tokio::test]
async fn wrapping_strips_stale_body_representation_headers() {
    // The foreign response claims a gzip-encoded, range-partial,
    // ETag-tagged body with its own `Content-Length`. After wrapping
    // replaces the body with plaintext `Problem` JSON, none of those
    // must survive verbatim: a client would try to gunzip the new
    // plaintext body (`Content-Encoding`), misinterpret it as a range
    // response (`Content-Range`), cache-validate against the wrong
    // representation (`ETag`), or truncate/hang trying to read
    // `Content-Length` bytes of a body that is actually a different
    // size (the case that matters most - `to_bytes` in this test
    // helper ignores `Content-Length` entirely, so a stale value would
    // pass unnoticed here without an explicit assertion on it).
    let app = build_foreign_app(|| {
        let mut response = foreign_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text/plain; charset=utf-8",
            "length limit exceeded",
        );
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-99/1000"),
        );
        headers.insert(header::ETAG, HeaderValue::from_static("\"foreign-etag\""));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999999"));
        response
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.headers().get(header::CONTENT_ENCODING), None);
    assert_eq!(res.headers().get(header::CONTENT_RANGE), None);
    assert_eq!(res.headers().get(header::ETAG), None);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(PROBLEM_JSON)
    );
    // Captured before consuming `res` below.
    let content_length: Option<usize> = res
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    // Body must be the plain, uncompressed Problem JSON - not gzip bytes
    // reinterpreted as JSON, which would fail to parse at all.
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Payload Too Large",
            "status": 413,
            "detail": "Payload Too Large",
            "instance": "/upload",
            "context": {},
        })
    );

    // `Content-Length` must equal the actual serialized Problem body's
    // byte length, not the foreign response's stale "999999".
    let expected_len = serde_json::to_vec(&json).unwrap().len();
    assert_eq!(content_length, Some(expected_len));
}

#[tokio::test]
async fn wrapping_preserves_every_set_cookie_value() {
    // `Set-Cookie` is repeatable (RFC 6265 §4.1.1 forbids folding
    // multiple cookies into one line) - a foreign error response that
    // clears a session and sets a replacement must keep both cookies,
    // not just the first, after wrapping replaces its body.
    let app = build_foreign_app(|| {
        let mut response = foreign_response(StatusCode::UNAUTHORIZED, "text/plain", "denied");
        let headers = response.headers_mut();
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("session=; Max-Age=0"),
        );
        headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("session=new-value; Path=/"),
        );
        response
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let cookies: Vec<&str> = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(
        cookies,
        vec!["session=; Max-Age=0", "session=new-value; Path=/"]
    );
}

#[tokio::test]
async fn wrapping_preserves_multiple_www_authenticate_challenges() {
    // RFC 9110 §11.6.1: a response can offer several auth challenges.
    // No call site in this codebase appends more than one today, but
    // this must not silently regress to single-valued handling.
    let app = build_foreign_app(|| {
        let mut response = foreign_response(StatusCode::UNAUTHORIZED, "text/plain", "denied");
        let headers = response.headers_mut();
        headers.append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic"));
        headers.append(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"api\""),
        );
        response
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let challenges: Vec<&str> = res
        .headers()
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(challenges, vec!["Basic", "Bearer realm=\"api\""]);
}

#[tokio::test]
async fn wrapping_preserves_rate_limit_headers_on_a_recovered_canonical_error() {
    // `gears/system/api-gateway/src/middleware/throttling.rs` and
    // `oagw`'s `error_response` both attach rate-limit/quota headers
    // (RateLimit-Policy, RateLimit-Limit, X-RateLimit-Limit/-Remaining/
    // -Reset) directly onto a `CanonicalError`/`Problem` response - the
    // same kind of response `wrap_recovered_canonical_error` handles
    // when its body is corrupted (see the test above). Without this
    // list, `Retry-After` alone would survive that path while its
    // sibling quota numbers silently vanished.
    use axum::response::IntoResponse;

    let app = Router::new()
        .route(
            "/api/v1/widgets/42",
            get(|| async {
                let mut response = CanonicalError::service_unavailable()
                    .with_detail("rate limit exceeded")
                    .create()
                    .into_response();
                let headers = response.headers_mut();
                headers.insert("ratelimit-policy", HeaderValue::from_static("10;w=60"));
                headers.insert("ratelimit-limit", HeaderValue::from_static("10"));
                headers.insert("x-ratelimit-limit", HeaderValue::from_static("10"));
                headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
                headers.insert("x-ratelimit-reset", HeaderValue::from_static("60"));
                *response.body_mut() = Body::from(&b"{not-json}"[..]);
                response
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let get = |name: &str| {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(get("ratelimit-policy"), Some("10;w=60".to_owned()));
    assert_eq!(get("ratelimit-limit"), Some("10".to_owned()));
    assert_eq!(get("x-ratelimit-limit"), Some("10".to_owned()));
    assert_eq!(get("x-ratelimit-remaining"), Some("0".to_owned()));
    assert_eq!(get("x-ratelimit-reset"), Some("60".to_owned()));
}

#[tokio::test]
async fn is_problem_response_is_case_and_parameter_insensitive() {
    // A validly-cased-but-non-lowercase `Content-Type` must still be
    // recognized as an existing Problem and take the enrichment path,
    // not be misclassified as foreign and generically re-wrapped.
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let app = build_foreign_app(move || {
        let body = serde_json::to_vec(&problem).expect("serialize problem");
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(
                header::CONTENT_TYPE,
                "Application/Problem+JSON; charset=utf-8",
            )
            .body(Body::from(body))
            .expect("build response")
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let json = body_to_json(res).await;

    // Enriched (instance filled from the real path, real internal
    // category preserved), not generically rewrapped (which would have
    // produced "about:blank" and dropped the internal category).
    assert_eq!(
        json,
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
async fn wraps_a_foreign_5xx_response_as_internal_not_about_blank() {
    // Unlike a 4xx, a foreign 5xx is unambiguous - it's this platform's
    // own fault - so it maps to the real `internal` canonical category
    // (DESIGN.md §2.1's fail-safe fallback) rather than `about:blank`.
    // The original status (502, not 500) is preserved via
    // `.with_override` - see `wrap_as_internal_problem`.
    let app = build_foreign_app(|| {
        foreign_response(
            StatusCode::BAD_GATEWAY,
            "text/plain; charset=utf-8",
            "upstream connection refused",
        )
    });

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(PROBLEM_JSON)
    );
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 502,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
async fn empty_foreign_body_falls_back_to_the_reason_phrase() {
    let app = build_foreign_app(|| foreign_response(StatusCode::NOT_FOUND, "text/plain", ""));

    let req = Request::builder()
        .uri("/upload")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Not Found",
            "status": 404,
            "detail": "Not Found",
            "instance": "/upload",
            "context": {},
        })
    );
}

#[tokio::test]
async fn wrapped_problem_gets_trace_id_from_headers() {
    let app = build_foreign_app(|| {
        foreign_response(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "too big")
    });

    let req = Request::builder()
        .uri("/upload")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Payload Too Large",
            "status": 413,
            "detail": "Payload Too Large",
            "instance": "/upload",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
            "context": {},
        })
    );
}

#[tokio::test]
async fn a_3xx_redirect_passes_through_unwrapped() {
    let app = Router::new()
        .route(
            "/moved",
            get(|| async {
                Response::builder()
                    .status(StatusCode::FOUND)
                    .header(header::LOCATION, "/new-location")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/moved")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(
        res.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/new-location")
    );
}

#[tokio::test]
async fn an_existing_problem_response_is_enriched_not_generically_rewrapped() {
    // If this ever regressed to routing an already-Problem response
    // through the generic-wrap fallback instead of the existing
    // enrichment path, `problem_type` would become "about:blank" here -
    // it must stay the real canonical category.
    let problem: Problem = CanonicalError::internal("boom").create().into();
    let expected_type = problem.problem_type.clone();
    let app = build_app(move || problem_response(&problem, StatusCode::INTERNAL_SERVER_ERROR));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let problem = body_to_problem(res).await;

    assert_eq!(problem.problem_type, expected_type);
    assert_ne!(problem.problem_type, "about:blank");
}

#[tokio::test]
async fn generic_wrap_fallback_also_fixes_an_unmigrated_bare_json_handler() {
    // design.md's Open Question: does the widened middleware already fix
    // a plain (unmigrated) `axum::Json<T>` handler's `JsonRejection`,
    // without that handler ever adopting `CanonicalJson<T>`? Answer:
    // YES - `canonical_error_middleware` wraps the whole router
    // (`next.run(request)` executes route matching AND extraction), so
    // it already sees `JsonRejection::into_response()`'s plain-text
    // output today; the only thing gating it was `is_problem_response`'s
    // pass-through, which the generic-wrap branch now replaces for any
    // error status. This test locks that behavior in: a bare `Json<T>`
    // handler, with NO `CanonicalJson` involved at all, still ends up
    // `application/problem+json` once this middleware is in the stack.
    use axum::Json;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Widget {
        #[allow(dead_code)]
        name: String,
    }

    let app = Router::new()
        .route(
            "/widgets",
            axum::routing::post(|Json(_w): Json<Widget>| async { StatusCode::CREATED }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .method("POST")
        .uri("/widgets")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"a","extra":1}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(PROBLEM_JSON)
    );
    let json = body_to_json(res).await;
    // Generic fallback, not `extract::Json`'s precise
    // `invalid_json_body` field violation - confirms the two pieces are
    // complementary, not redundant: this handler gets RFC-shape
    // correctness for free (about:blank, a generic reason-phrase
    // detail), but neither axum's specific diagnostic message nor the
    // structured machine-readable reason code `extract::Json` would
    // have given it - the fallback never puts foreign response content
    // in the client-visible `detail` (see `wrap_as_generic_problem`).
    assert_eq!(
        json,
        json!({
            "type": "about:blank",
            "title": "Unprocessable Entity",
            "status": 422,
            "detail": "Unprocessable Entity",
            "instance": "/widgets",
            "context": {},
        })
    );
}

#[tokio::test]
async fn a_deliberately_shaped_json_error_body_is_left_alone() {
    // Regression test for a real bug: a handler that predates this
    // platform's RFC 9457 adoption and returns its own custom JSON error
    // shape (e.g. `api-gateway`'s `/health`, which returns
    // `{"status", "timestamp", "components"}` with a 503) must keep that
    // body verbatim - `Content-Type: application/json` is not the
    // "genuinely unstructured" `text/plain`/missing case this fallback
    // exists to rescue (see `is_unstructured_error_body`), so it must not
    // be silently replaced with a generic `internal` Problem, destroying
    // fields real clients rely on.
    let app = Router::new()
        .route(
            "/health",
            axum::routing::get(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(json!({
                        "status": "unhealthy",
                        "components": [{"code": "db_unreachable"}],
                    })),
                )
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json = body_to_json(res).await;
    assert_eq!(
        json,
        json!({
            "status": "unhealthy",
            "components": [{"code": "db_unreachable"}],
        })
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn logs_internal_description_from_extension() {
    // `Internal::description` is `#[serde(skip)]` so the wire body cannot
    // carry the unredacted message. The middleware recovers the original
    // `CanonicalError` from response extensions (DESIGN §3.6) and logs
    // the diagnostic server-side; the live span's `trace_id` is spliced onto
    // the log record by the correlation formatter, not by this event.
    use axum::response::IntoResponse;

    let app = Router::new()
        .route(
            "/api/v1/widgets/42",
            get(|| async {
                CanonicalError::internal("db connection refused: secret-host:5432")
                    .create()
                    .into_response()
            }),
        )
        .layer(from_fn(canonical_error_middleware));

    let req = Request::builder()
        .uri("/api/v1/widgets/42")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // The wire body must NOT contain the diagnostic.
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = std::str::from_utf8(&bytes).unwrap();
    assert!(
        !body_str.contains("secret-host:5432"),
        "diagnostic must not appear on the wire"
    );

    // Server-side log must contain the diagnostic.
    assert!(logs_contain("canonical error response (server)"));
    assert!(logs_contain("db connection refused: secret-host:5432"));
}

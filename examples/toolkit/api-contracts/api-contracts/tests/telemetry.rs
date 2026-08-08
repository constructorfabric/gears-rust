//! Proves the macro-generated REST client bakes in telemetry: every unary
//! method opens a `tracing` client span named `{Trait}.{method}` carrying the
//! OTel-semantic fields (`otel.kind`, `rpc.system`, `rpc.service`, `rpc.method`,
//! `http.method`, `http.route`). That span is what makes `toolkit-http`'s
//! `OtelLayer` inject the correct W3C `traceparent` downstream — see ADR-0006.
//!
//! We capture spans with a minimal `tracing_subscriber` layer rather than a
//! live collector: this asserts the *client-side* instrumentation exists and is
//! well-formed without requiring the `toolkit-http/otel` feature (which governs
//! only the wire-level `traceparent` injection).

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::ChargeRequest;
use api_contracts_sdk::rest::PaymentApiRestClient;
use axum::Router;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_contract::policy::PolicyStack;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_security::SecurityContext;

use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Id, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

use cf_api_contracts::api::rest::routes::register_routes;
use cf_api_contracts::client::local::PaymentLocalClient;
use cf_api_contracts::domain::service::PaymentDomainService;

/// A span captured by [`CaptureLayer`]: its metadata name plus recorded fields.
#[derive(Clone, Debug)]
struct CapturedSpan {
    name: String,
    fields: HashMap<String, String>,
}

/// Collects `on_new_span` events into a shared vector.
struct CaptureLayer {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

struct FieldVisitor<'a>(&'a mut HashMap<String, String>);

impl Visit for FieldVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .entry(field.name().to_owned())
            .or_insert_with(|| format!("{value:?}"));
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = HashMap::new();
        attrs.record(&mut FieldVisitor(&mut fields));
        self.spans.lock().push(CapturedSpan {
            name: attrs.metadata().name().to_owned(),
            fields,
        });
    }
}

async fn start_test_server() -> String {
    let svc = Arc::new(PaymentDomainService::new());
    let local: Arc<dyn PaymentApi> =
        Arc::new(PaymentLocalClient::new(svc, Arc::new(PolicyStack::new())));

    let secctx_layer = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(SecurityContext::anonymous());
            next.run(req).await
        },
    );

    let openapi = OpenApiRegistryImpl::new();
    let app = register_routes(Router::new(), &openapi, local).layer(secctx_layer);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn generated_client_emits_named_contract_span() {
    let spans = Arc::new(Mutex::new(Vec::<CapturedSpan>::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer {
        spans: Arc::clone(&spans),
    });

    // `#[tokio::test]` uses a current-thread runtime, so the thread-local
    // default subscriber is visible to the client's spawned dispatch.
    let _guard = tracing::subscriber::set_default(subscriber);

    let base_url = start_test_server().await;
    let client = PaymentApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    let resp = PaymentApi::charge(
        &client,
        SecurityContext::anonymous(),
        ChargeRequest::new(1000, "USD", "telemetry test"),
    )
    .await
    .unwrap();
    assert!(!resp.payment_id.is_nil());

    let captured = spans.lock().clone();
    let charge_span = captured
        .iter()
        .find(|s| s.name == "PaymentApiRest.charge")
        .unwrap_or_else(|| {
            panic!(
                "expected a `PaymentApiRest.charge` client span; captured: {:?}",
                captured.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });

    assert_eq!(
        charge_span.fields.get("otel.kind").map(String::as_str),
        Some("client")
    );
    assert_eq!(
        charge_span.fields.get("rpc.system").map(String::as_str),
        Some("rest")
    );
    assert_eq!(
        charge_span.fields.get("rpc.service").map(String::as_str),
        Some("PaymentApiRest")
    );
    assert_eq!(
        charge_span.fields.get("rpc.method").map(String::as_str),
        Some("charge")
    );
    assert_eq!(
        charge_span.fields.get("http.method").map(String::as_str),
        Some("POST")
    );
    assert_eq!(
        charge_span.fields.get("http.route").map(String::as_str),
        Some("/payments/charge"),
    );
}

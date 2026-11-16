#![allow(clippy::expect_used, clippy::panic)]

//! `IdP` calls get toolkit-http's client span (and, with its `otel` feature,
//! a `traceparent` header taken from that span).
//!
//! Its own test binary: the subscriber is process-global, which a thread-local
//! one sharing a binary with parallel tests is not reliably.

use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt;

pub mod common;

/// Records the names of all spans opened in the process.
#[derive(Clone, Default)]
struct SpanNames(Arc<Mutex<Vec<&'static str>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanNames {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.0
            .lock()
            .expect("span log")
            .push(attrs.metadata().name());
    }
}

#[tokio::test]
async fn idp_calls_get_a_client_span() {
    let names = SpanNames::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(names.clone()))
        .expect("only test in this binary sets the subscriber");

    let server = common::MockOidcServer::spawn().expect("mock IdP");
    let issuer = server.issuer("realms/platform");
    let plugin = common::build_test_plugin(
        common::base_jwt_validation_config(),
        common::exact_issuer_trust(issuer.clone()).expect("trust config"),
        common::plugin_config(),
        oidc_authn_plugin::infra::idp_http::IdpHttpClient::insecure_for_tests(),
    );
    let token = common::sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
            "iss": issuer,
            "exp": common::future_exp(),
        }),
        Some(common::TEST_KID),
    );

    plugin.authenticate(&token).await.expect("token validates");

    let client_spans = names
        .0
        .lock()
        .expect("span log")
        .iter()
        .filter(|name| **name == "outgoing_http")
        .count();
    assert_eq!(client_spans, 2, "one per IdP call: discovery and JWKS");
}

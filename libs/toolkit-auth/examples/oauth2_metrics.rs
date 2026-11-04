//! `OAuth2` client with OpenTelemetry request metrics.
//!
//! This is the Rust equivalent of go-appkit's `httpclient` metrics round
//! tripper: every logical request records a single duration histogram
//! (`http.client.request.duration`, seconds) tagged with the HTTP method, a
//! bounded route label, the server address, and the status code (or
//! `error.type` on a transport failure).
//!
//! Two ways to enable it:
//! - [`with_metrics`] — default classifier, labels each request
//!   `"METHOD host"` (like go-appkit's default `summary`).
//! - [`with_metrics_by`] — your own classifier that collapses
//!   parameterized paths into route templates, exactly like go-appkit's
//!   `ClassifyRequest`. This is what keeps metric cardinality bounded: never
//!   let a raw path (with IDs/UUIDs) become a label.
//!
//! Requires `toolkit-http` built with the `otel` feature. The application must
//! also install an OpenTelemetry `MeterProvider` at startup (e.g. a Prometheus
//! exporter); without one these calls are cheap no-ops. Under a Prometheus
//! exporter the histogram surfaces as `http_client_request_duration_seconds`.
//!
//! NOTE: Requires a running IDP. Meant as an API reference, not a runnable demo.

use std::borrow::Cow;

use toolkit_auth::{HttpClientBuilderExt, OAuthClientConfig, SecretString, TokenProvider};
use toolkit_http::HttpClientBuilder;

/// Demonstrate provider construction and authenticated requests to an external service.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Step 1 -- create the token provider (see oauth2_basic.rs).
    let token = TokenProvider::new(OAuthClientConfig {
        token_endpoint: Some("https://idp.example.com/oauth/token".parse()?),
        client_id: "tenants-resolver".into(),
        client_secret: SecretString::new("my-secret"),
        scopes: vec!["tenants.read".into()],
        ..Default::default()
    })?;

    // Step 2a -- simplest form: Bearer auth + default metrics.
    // Routes are labeled "METHOD host", e.g. `GET resolver.example.com`.
    let _simple_client = HttpClientBuilder::new()
        .with_bearer_auth(token.clone())
        .with_metrics("tenants-resolver")
        .build()?;

    // Step 2b -- production form: a custom classifier turns parameterized paths
    // into bounded route templates so per-tenant IDs never explode the
    // `http.route` cardinality. Mirrors go-appkit's `ClassifyRequest`.
    let client = HttpClientBuilder::new()
        .with_bearer_auth(token)
        .with_metrics_by("tenants-resolver", |req| {
            let path = req.uri().path();
            // Collapse `/api/v1/tenants/<id>` -> a single template; keep the
            // collection path as-is. Returning a small, fixed set of strings is
            // the whole point — anything derived from request data must be
            // bucketed here, not passed through verbatim.
            if path.starts_with("/api/v1/tenants/") {
                Cow::Borrowed("/api/v1/tenants/{id}")
            } else if path == "/api/v1/tenants" {
                Cow::Borrowed("/api/v1/tenants")
            } else {
                Cow::Borrowed("other")
            }
        })
        .build()?;

    // Step 3 -- each request records one duration observation. Retries are
    // collapsed into a single logical observation (the metrics layer sits
    // outside the retry loop).
    let _resp = client
        .get("https://resolver.example.com/api/v1/tenants/123")
        .send()
        .await?;

    println!(
        "Request sent; recorded http.client.request.duration{{http.route=/api/v1/tenants/{{id}}}}"
    );
    Ok(())
}

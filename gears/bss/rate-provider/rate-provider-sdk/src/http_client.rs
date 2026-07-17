//! Shared outbound HTTP client construction for rate-provider sources.

use std::time::Duration;

use toolkit_http::HttpClient;

/// Build the shared outbound HTTP client for a rate-provider source:
/// TLS-only, a per-attempt timeout, `OTel`-instrumented. Validates `base_url`
/// is `https://` up front, so a plain-`http` misconfiguration fails at gear
/// `init()` — never silently deferred to the first fetch.
///
/// `base_url` is never echoed into the returned error: it may have an
/// operator-embedded secret spliced into it (no rate-provider source
/// supports query-string API-key auth, only header-based auth).
///
/// # Errors
/// An error if `base_url` does not start with `https://`, or if the
/// underlying client fails to build.
pub fn build_source_http_client(base_url: &str, timeout_ms: u64) -> anyhow::Result<HttpClient> {
    anyhow::ensure!(
        base_url.starts_with("https://"),
        "base_url must start with https://"
    );
    Ok(HttpClient::builder()
        .deny_insecure_http()
        .timeout(Duration::from_millis(timeout_ms))
        .with_otel()
        .build()?)
}

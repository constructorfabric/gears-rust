//! Shared outbound HTTP client construction for rate-provider sources.

use std::time::Duration;

use http::Uri;
use toolkit_http::HttpClient;

/// Largest accepted per-attempt outbound timeout (30 s).
///
/// A ceiling, not a recommendation: the plugin defaults are 5 s. It exists to
/// reject a misconfigured source that would stall a whole rate-sync pass — see
/// [`build_source_http_client`].
pub const MAX_TIMEOUT_MS: u64 = 30_000;

/// Build the shared outbound HTTP client for a rate-provider source:
/// TLS-only, a per-attempt timeout, `OTel`-instrumented. Validates `base_url` up
/// front, so a misconfiguration fails at gear `init()` — never silently deferred
/// to the first fetch.
///
/// The URL is parsed with the same [`Uri`] type `toolkit-http` uses on the request
/// path, rather than string-matched, so this gate applies the upstream
/// normalization rules: the scheme is compared **case-insensitively** (`Uri`
/// canonicalizes it, so `HTTPS://host` is accepted) and a **host is required**
/// (`https://` alone is not a usable endpoint).
///
/// A zero timeout is not "no timeout": it becomes a zero-duration per-attempt
/// deadline that elapses before any connect can finish, so every request — and
/// every retry — fails at the first fetch. That is the deferred failure this
/// function exists to prevent, so it is rejected too.
///
/// The upper bound exists because the composite fetches sources **sequentially**,
/// making the worst-case duration of one rate-sync pass the sum of every source's
/// `timeout_ms`. That sum has to stay well under the ledger's rate-sync interval
/// (1 h by default), or a pass overruns its schedule and refreshes are silently
/// skipped. No single `init()` can check the real rule — the timeouts live in each
/// source plugin's config, the interval in the ledger's — but this gate does catch
/// the input that actually happens: a unit mix-up (seconds written into a
/// milliseconds field) turning one source into a minutes-long stall.
///
/// `base_url` is never echoed into the returned error: it may have an
/// operator-embedded secret spliced into it (no rate-provider source supports
/// query-string API-key auth, only header-based auth).
///
/// # Errors
/// An error if `base_url` is not a parseable URL, its scheme is not https, it
/// carries no host, `timeout_ms` is zero or above [`MAX_TIMEOUT_MS`], or the
/// underlying client fails to build.
pub fn build_source_http_client(base_url: &str, timeout_ms: u64) -> anyhow::Result<HttpClient> {
    // The parse error from `http` reports the malformation only, never the input.
    let uri: Uri = base_url
        .parse()
        .map_err(|e| anyhow::anyhow!("base_url is not a valid URL: {e}"))?;
    anyhow::ensure!(
        uri.scheme_str() == Some("https"),
        "base_url must use the https scheme"
    );
    anyhow::ensure!(uri.authority().is_some(), "base_url must include a host");
    anyhow::ensure!(timeout_ms > 0, "timeout_ms must be greater than zero");
    // The ceiling is named in the message so the operator can see the limit
    // without reading this source.
    anyhow::ensure!(
        timeout_ms <= MAX_TIMEOUT_MS,
        "timeout_ms must not exceed {MAX_TIMEOUT_MS} ms"
    );
    Ok(HttpClient::builder()
        .deny_insecure_http()
        .timeout(Duration::from_millis(timeout_ms))
        .with_otel()
        .build()?)
}

#[cfg(test)]
#[path = "http_client_tests.rs"]
mod tests;

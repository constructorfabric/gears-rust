//! Shared instrumented HTTP GET + parse for every rate-provider source: timing,
//! upstream-status, and fetch-error metrics, plus a `tracing` line on every
//! failure branch. Error-kind labels are derived from the mapped
//! [`RateProviderError`] (never a hardcoded guess made before the error is
//! actually known).
//!
//! On the two transport branches the `warn` line carries only that fixed kind
//! label and the error text is emitted at `debug`, so the level operators alert
//! on has a bounded vocabulary — see [`crate::error::map_http_error`].

use std::time::Instant;

use bss_ledger_sdk::{ProviderRate, RateProviderError};
use toolkit_http::RequestBuilder;

use crate::error::{error_kind_label, map_http_error};
use crate::metrics::FetchMetrics;

/// Send `request`, then hand the response bytes to `parse`. `parse` returns the
/// mapped rates plus the publication timestamp to record as the "last success"
/// gauge — a *feed-freshness* signal (the document's own publication time), not
/// wall-clock fetch time.
///
/// # Errors
/// Propagates a transport failure ([`RateProviderError::Unreachable`] /
/// [`RateProviderError::Internal`]), a non-2xx status
/// ([`RateProviderError::UpstreamStatus`]), or whatever `parse` returns.
pub async fn fetch_and_parse<F>(
    request: RequestBuilder,
    provider_id: &str,
    metrics: &dyn FetchMetrics,
    parse: F,
) -> Result<Vec<ProviderRate>, RateProviderError>
where
    F: FnOnce(&[u8]) -> Result<(Vec<ProviderRate>, i64), RateProviderError>,
{
    let started = Instant::now();
    let resp = request.send().await.map_err(|e| {
        let mapped = map_http_error(&e);
        let kind = error_kind_label(&mapped);
        // `warn` carries only the fixed kind label; the transport text goes to
        // `debug`. `HttpError::Transport`/`Tls` forward an arbitrary inner error's
        // `Display`, and `HttpError` is `#[non_exhaustive]`, so the strings that
        // can arrive here are not a closed set — the line an operator alerts on
        // stays a bounded vocabulary. No credential can appear on either line:
        // every source authenticates with a header, never a query string.
        tracing::warn!(provider = provider_id, error_kind = kind, "rate-provider source: request failed");
        tracing::debug!(provider = provider_id, error = %mapped, "rate-provider source: request failure detail");
        metrics.incr_fetch_error(provider_id, kind);
        mapped
    })?;

    let status = resp.status().as_u16();
    metrics.incr_upstream_status(provider_id, status);
    if !resp.status().is_success() {
        tracing::warn!(
            provider = provider_id,
            status,
            "rate-provider source: upstream returned a non-success status"
        );
        metrics.incr_fetch_error(provider_id, "upstream_status");
        return Err(RateProviderError::UpstreamStatus(status));
    }

    let bytes = resp.bytes().await.map_err(|e| {
        let mapped = map_http_error(&e);
        let kind = error_kind_label(&mapped);
        // Same split as the send branch above.
        tracing::warn!(provider = provider_id, error_kind = kind, "rate-provider source: reading response body failed");
        tracing::debug!(provider = provider_id, error = %mapped, "rate-provider source: body-read failure detail");
        metrics.incr_fetch_error(provider_id, kind);
        mapped
    })?;

    let (rates, as_of_unix) = parse(&bytes).inspect_err(|e| {
        tracing::warn!(provider = provider_id, error = %e, "rate-provider source: parsing response body failed");
        metrics.incr_fetch_error(provider_id, error_kind_label(e));
    })?;

    metrics.observe_fetch(provider_id, started.elapsed().as_secs_f64());
    metrics.observe_rates_returned(provider_id, u64::try_from(rates.len()).unwrap_or(u64::MAX));
    metrics.set_last_success(provider_id, as_of_unix);
    Ok(rates)
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;

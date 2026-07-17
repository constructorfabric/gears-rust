//! Shared instrumented HTTP GET + parse for every rate-provider source: timing,
//! upstream-status, and fetch-error metrics, plus a `tracing` line on every
//! failure branch. Error-kind labels are derived from the mapped
//! [`RateProviderError`] (never a hardcoded guess made before the error is
//! actually known).

use std::time::Instant;

use bss_ledger_sdk::{ProviderRate, RateProviderError};
use toolkit_http::RequestBuilder;

use crate::error::{error_kind_label, map_http_error};
use crate::metrics::FetchMetrics;

/// Send `request`, then hand the response bytes to `parse`. `parse` returns
/// the mapped rates plus the publication timestamp to record as the
/// "last success" gauge — a *feed-freshness* signal (the served document's
/// own publication time), not wall-clock fetch time, matching every source's
/// existing behavior.
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
        tracing::warn!(provider = provider_id, error = %mapped, "rate-provider source: request failed");
        metrics.incr_fetch_error(provider_id, error_kind_label(&mapped));
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
        tracing::warn!(provider = provider_id, error = %mapped, "rate-provider source: reading response body failed");
        metrics.incr_fetch_error(provider_id, error_kind_label(&mapped));
        mapped
    })?;

    let (rates, as_of_unix) = parse(&bytes).inspect_err(|e| {
        tracing::warn!(provider = provider_id, error = %e, "rate-provider source: parsing response body failed");
        metrics.incr_fetch_error(provider_id, "internal");
    })?;

    metrics.observe_fetch(provider_id, started.elapsed().as_secs_f64());
    metrics.observe_rates_returned(provider_id, u64::try_from(rates.len()).unwrap_or(u64::MAX));
    metrics.set_last_success(provider_id, as_of_unix);
    Ok(rates)
}

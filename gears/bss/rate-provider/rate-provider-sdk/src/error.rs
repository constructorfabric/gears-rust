//! Maps `toolkit_http::HttpError` onto the ledger contract's `RateProviderError`.
//! Shared by every rate-provider source plugin.
//!
//! Transport-level failures (timeout, DNS, TLS, connection) become
//! [`RateProviderError::Unreachable`]; a decoded non-success HTTP status becomes
//! [`RateProviderError::UpstreamStatus`]; everything else (bad URL, oversized
//! body, JSON decode fault) is a local [`RateProviderError::Internal`]. Never a
//! silent success.

use bss_ledger_sdk::RateProviderError;
use toolkit_http::HttpError;

/// Map a `toolkit_http` transport error to the ledger's semantic rate error.
///
/// Note: a non-2xx status only surfaces as [`HttpError::HttpStatus`] when the
/// caller uses a status-checking body reader (`.json()` / `.checked_bytes()`);
/// the sources call plain `.send()` and inspect `response.status()` directly,
/// mapping to [`RateProviderError::UpstreamStatus`] themselves.
#[must_use]
pub fn map_http_error(e: &HttpError) -> RateProviderError {
    match e {
        HttpError::Timeout(_) | HttpError::DeadlineExceeded(_) => {
            RateProviderError::Unreachable(e.to_string())
        }
        HttpError::HttpStatus { status, .. } => RateProviderError::UpstreamStatus(status.as_u16()),
        HttpError::Transport(_)
        | HttpError::Tls(_)
        | HttpError::Overloaded
        | HttpError::ServiceClosed => RateProviderError::Unreachable(e.to_string()),
        // `InvalidUri`'s `Display` embeds the raw request URL verbatim. A
        // `base_url` may have an operator-embedded secret spliced into it (no
        // source here supports query-string API-key auth), so never echo the
        // URL itself into an error the sync job logs/alarms on — only the
        // structured `kind` + diagnostic `reason`.
        HttpError::InvalidUri { kind, reason, .. } => {
            RateProviderError::Internal(format!("invalid request URL ({kind:?}): {reason}"))
        }
        // `HttpError` is `#[non_exhaustive]`; local faults + any future variant
        // fall through here as an internal fault (never a silent success).
        other => RateProviderError::Internal(other.to_string()),
    }
}

/// Metric-label classification of a mapped [`RateProviderError`], for
/// `FetchMetrics::incr_fetch_error`. Kept separate from the error's own
/// `Display` so the metric label always reflects the *real* mapped kind,
/// never a hardcoded guess made before `map_http_error` ran.
#[must_use]
pub fn error_kind_label(e: &RateProviderError) -> &'static str {
    match e {
        RateProviderError::PairUnavailable { .. } => "pair_unavailable",
        RateProviderError::Unreachable(_) => "unreachable",
        RateProviderError::UpstreamStatus(_) => "upstream_status",
        RateProviderError::InvalidPair(_) => "invalid_pair",
        RateProviderError::Internal(_) => "internal",
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;

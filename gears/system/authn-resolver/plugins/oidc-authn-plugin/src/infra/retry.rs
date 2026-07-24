//! Shared retry helpers for outbound `IdP` HTTP calls.

use std::error::Error as _;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use tokio_retry::RetryIf;
use tokio_retry::strategy::{ExponentialBackoff, jitter};

use crate::config::RetryPolicyConfig;

/// Terminal failure returned after retry policy handling is exhausted or skipped.
#[derive(Debug)]
pub enum RetriedRequestError {
    /// A non-retryable transport error, or a retryable one after all retries.
    Transport(reqwest::Error),
    /// A non-success HTTP status, retryable or not, after policy handling.
    Status(StatusCode),
}

/// Returns `true` when an HTTP status is retryable under policy.
#[must_use]
pub fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// Returns `true` when a transport error is retryable under policy.
#[must_use]
pub fn is_retryable_transport(error: &reqwest::Error) -> bool {
    if error.is_timeout()
        || error.is_builder()
        || error.is_redirect()
        || error.is_status()
        || error.is_body()
        || error.is_decode()
    {
        return false;
    }

    error.is_connect() || has_transient_io_source(error)
}

fn has_transient_io_source(error: &reqwest::Error) -> bool {
    let mut source = error.source();

    while let Some(err) = source {
        if let Some(io_error) = err.downcast_ref::<io::Error>()
            && matches!(
                io_error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            )
        {
            return true;
        }

        source = err.source();
    }

    false
}

/// Parse `Retry-After` delta-seconds or HTTP-date value and cap to `max_backoff`.
#[must_use]
pub fn retry_after_delay(response: &reqwest::Response, max_backoff: Duration) -> Option<Duration> {
    let raw = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let raw = raw.to_str().ok()?;
    parse_retry_after_delay(raw, SystemTime::now(), max_backoff)
}

/// Send a request and apply the shared outbound `IdP` retry policy.
///
/// The closure is called once per attempt so callers can rebuild non-cloneable
/// request builders while this helper owns retry classification and sleeping.
pub async fn send_with_retry<F, Fut>(
    policy: &RetryPolicyConfig,
    mut send: F,
) -> Result<reqwest::Response, RetriedRequestError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<reqwest::Response>>,
{
    // Server `Retry-After` (HTTP 429) overrides our computed backoff for the
    // next delay. The action writes the parsed value (ms; `0` = none) here and
    // the strategy consumes it, so the override rides inside tokio-retry instead
    // of a separate bespoke sleep.
    let retry_after_ms = Arc::new(AtomicU64::new(0));

    let jitter_on = policy.jitter;
    let strat_cell = Arc::clone(&retry_after_ms);
    let strategy = ExponentialBackoff::from_millis(policy.backoff_base_ms)
        .factor(policy.backoff_factor)
        .max_delay(policy.max_backoff)
        .map(move |computed| {
            let override_ms = strat_cell.swap(0, Ordering::Relaxed);
            if override_ms > 0 {
                // `Retry-After` is server-authoritative — used verbatim (already
                // capped to `max_backoff`), never jittered.
                Duration::from_millis(override_ms)
            } else if jitter_on {
                jitter(computed)
            } else {
                computed
            }
        })
        .take(policy.max_retries as usize);

    let max_backoff = policy.max_backoff;
    let action_cell = Arc::clone(&retry_after_ms);
    let action = || {
        let cell = Arc::clone(&action_cell);
        let fut = send();
        async move {
            match fut.await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    if status == StatusCode::TOO_MANY_REQUESTS
                        && let Some(delay) = retry_after_delay(&response, max_backoff)
                    {
                        let ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
                        // Clamp to >=1 so `0` stays the "no override" sentinel.
                        cell.store(ms.max(1), Ordering::Relaxed);
                    }
                    Err(RetriedRequestError::Status(status))
                }
                Err(error) => Err(RetriedRequestError::Transport(error)),
            }
        }
    };

    // Classify which failures are worth retrying; everything else short-circuits.
    let retryable = |e: &RetriedRequestError| match e {
        RetriedRequestError::Transport(error) => is_retryable_transport(error),
        RetriedRequestError::Status(status) => is_retryable_status(*status),
    };

    RetryIf::start(strategy, action, retryable).await
}

fn parse_retry_after_delay(raw: &str, now: SystemTime, max_backoff: Duration) -> Option<Duration> {
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs).min(max_backoff));
    }

    let retry_at = httpdate::parse_http_date(raw).ok()?;
    let delay = retry_at.duration_since(now).unwrap_or(Duration::ZERO);
    Some(delay.min(max_backoff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_retry_after_delta_seconds() {
        let delay = parse_retry_after_delay("42", SystemTime::UNIX_EPOCH, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::from_secs(42)));
    }

    #[test]
    fn caps_retry_after_delta_seconds() {
        let delay = parse_retry_after_delay("120", SystemTime::UNIX_EPOCH, Duration::from_secs(3));

        assert_eq!(delay, Some(Duration::from_secs(3)));
    }

    #[test]
    fn parses_retry_after_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now + Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::from_secs(30)));
    }

    #[test]
    fn caps_retry_after_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now + Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_secs(3));

        assert_eq!(delay, Some(Duration::from_secs(3)));
    }

    #[test]
    fn treats_past_retry_after_http_date_as_zero_delay() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let retry_at = now - Duration::from_secs(30);
        let raw = httpdate::fmt_http_date(retry_at);

        let delay = parse_retry_after_delay(&raw, now, Duration::from_mins(2));

        assert_eq!(delay, Some(Duration::ZERO));
    }

    #[test]
    fn rejects_invalid_retry_after_value() {
        let delay =
            parse_retry_after_delay("not-a-date", SystemTime::UNIX_EPOCH, Duration::from_mins(2));

        assert_eq!(delay, None);
    }
}

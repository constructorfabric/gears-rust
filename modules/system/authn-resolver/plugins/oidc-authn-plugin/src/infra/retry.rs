//! Shared retry helpers for outbound `IdP` HTTP calls.

use std::error::Error as _;
use std::future::Future;
use std::io;
use std::time::{Duration, SystemTime};

use rand::RngExt as _;
use reqwest::StatusCode;

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

/// Compute exponential backoff for `retry_index` (1-based).
#[must_use]
pub fn compute_backoff(policy: &RetryPolicyConfig, retry_index: u32) -> Duration {
    let shift = retry_index.saturating_sub(1).min(20);
    let multiplier = 1_u64 << shift;
    let computed_ms = policy
        .initial_backoff_ms
        .saturating_mul(multiplier)
        .min(policy.max_backoff_ms);
    Duration::from_millis(computed_ms)
}

/// Apply full-jitter in `[0, upper]` when enabled.
#[must_use]
pub fn apply_jitter(upper: Duration, enabled: bool) -> Duration {
    if !enabled {
        return upper;
    }
    let upper_ms = u64::try_from(upper.as_millis()).unwrap_or(u64::MAX);
    if upper_ms <= 1 {
        return upper;
    }
    let jitter_ms = rand::rng().random_range(0..=upper_ms);
    Duration::from_millis(jitter_ms)
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
    let mut attempt = 0_u32;
    loop {
        let response = match send().await {
            Ok(response) => response,
            Err(error) => {
                if let Some(delay) = transport_retry_delay(policy, &mut attempt, &error) {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(RetriedRequestError::Transport(error));
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        if let Some(delay) = status_retry_delay(policy, &mut attempt, &response) {
            tokio::time::sleep(delay).await;
            continue;
        }

        return Err(RetriedRequestError::Status(status));
    }
}

fn transport_retry_delay(
    policy: &RetryPolicyConfig,
    attempt: &mut u32,
    error: &reqwest::Error,
) -> Option<Duration> {
    if !is_retryable_transport(error) || *attempt >= policy.max_attempts {
        return None;
    }

    *attempt += 1;
    Some(jittered_backoff(policy, *attempt))
}

fn status_retry_delay(
    policy: &RetryPolicyConfig,
    attempt: &mut u32,
    response: &reqwest::Response,
) -> Option<Duration> {
    let status = response.status();
    if !is_retryable_status(status) || *attempt >= policy.max_attempts {
        return None;
    }

    *attempt += 1;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Some(
            retry_after_delay(response, policy.max_backoff())
                .unwrap_or_else(|| jittered_backoff(policy, *attempt)),
        );
    }

    Some(jittered_backoff(policy, *attempt))
}

fn jittered_backoff(policy: &RetryPolicyConfig, retry_index: u32) -> Duration {
    apply_jitter(compute_backoff(policy, retry_index), policy.jitter)
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

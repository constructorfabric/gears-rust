//! Bounded exponential-backoff retry helper used by generated clients.

use std::future::Future;
use std::time::Duration;

use rand::RngExt;

use crate::runtime::config::RetryConfig;
use crate::runtime::transport_error::TransportError;

/// Run `f` with bounded exponential backoff while it returns transient errors.
///
/// Non-transient errors short-circuit immediately. The total number of
/// invocations is capped by [`RetryConfig::max_attempts`].
///
/// # Errors
/// Returns the last [`TransportError`] produced by `f` once retries are
/// exhausted, or the first non-transient error short-circuited from `f`.
pub async fn retry_with_backoff<F, Fut, T>(
    config: &RetryConfig,
    mut f: F,
) -> Result<T, TransportError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
{
    let mut attempt: u32 = 0;
    let max = config.max_attempts.max(1);
    loop {
        let result = f().await;
        attempt += 1;

        match result {
            Ok(v) => return Ok(v),
            Err(err) if attempt >= max => return Err(err),
            Err(err) if !err.is_transient() => return Err(err),
            Err(_) => {
                let delay = compute_delay(config, attempt);
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

fn compute_delay(config: &RetryConfig, attempt: u32) -> Duration {
    let base_secs = config.base_delay.as_secs_f64();
    let exp_i32 = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
    let exp = config.multiplier.powi(exp_i32);
    let target = base_secs * exp;
    let max_secs = config.max_delay.as_secs_f64();
    let capped = target.min(max_secs);
    if capped <= 0.0 {
        return Duration::ZERO;
    }
    // Floor jitter at `base_delay` so retries never collapse to ~0 while a
    // non-zero base is configured (degenerate config where base > capped is
    // clamped down to capped).
    let floor = base_secs.min(capped);
    if !floor.is_finite() || !capped.is_finite() {
        return if config.base_delay.as_secs_f64().is_finite() {
            config.base_delay
        } else {
            Duration::ZERO
        };
    }
    let jitter: f64 = rand::rng().random_range(floor..=capped);
    Duration::from_secs_f64(jitter)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn fast_config(max_attempts: u32) -> RetryConfig {
        RetryConfig {
            max_attempts,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
            multiplier: 1.0,
        }
    }

    #[tokio::test]
    async fn returns_first_success() {
        let cfg = fast_config(3);
        let result = retry_with_backoff(&cfg, || async { Ok::<_, TransportError>(42_u32) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let cfg = fast_config(4);
        let counter = Arc::new(AtomicU32::new(0));
        let result = retry_with_backoff(&cfg, || {
            let counter = Arc::clone(&counter);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(TransportError::network("transient"))
                } else {
                    Ok::<_, TransportError>("ok")
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let cfg = fast_config(2);
        let counter = Arc::new(AtomicU32::new(0));
        let result: Result<&str, _> = retry_with_backoff(&cfg, || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(TransportError::network("nope"))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn compute_delay_handles_non_finite_inputs() {
        // `Duration::from_secs_f64(NaN)` panics on construction, so we
        // exercise the NaN-guard path indirectly by feeding a NaN
        // `multiplier`. With the guard in place, `compute_delay` must
        // not panic regardless of the (non-finite) arithmetic.
        let cfg = RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: f64::NAN,
        };
        let _ = compute_delay(&cfg, 2);

        let cfg_inf = RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(1),
            multiplier: f64::INFINITY,
        };
        let _ = compute_delay(&cfg_inf, 2);
    }

    #[tokio::test]
    async fn does_not_retry_non_transient() {
        let cfg = fast_config(5);
        let counter = Arc::new(AtomicU32::new(0));
        let result: Result<(), _> = retry_with_backoff(&cfg, || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(TransportError::serialization("permanent"))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

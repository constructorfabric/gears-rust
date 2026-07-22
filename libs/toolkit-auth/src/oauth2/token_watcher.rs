//! Background token refresh with jitter and exponential backoff.
//!
//! [`TokenWatcher`] performs an initial token fetch, then spawns a background
//! `tokio` task that proactively refreshes the token before it goes stale.
//! The latest token is stored in an [`ArcSwap`] for lock-free reads.
//!
//! The public surface is intentionally minimal — only [`CachedToken`],
//! [`TokenWatcher`], and [`TokenStatus`] are used by the rest of the crate.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use rand::RngExt as _;
use toolkit_utils::SecretString;

/// Freshness status of a cached token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    /// Token is within its freshness window — safe to use, no refresh needed.
    Fresh,
    /// Token is past its freshness window but not yet expired — still usable,
    /// but a background refresh should be in progress.
    Stale,
    /// Token has passed its expiry time — must not be used.
    Expired,
}

/// A cached bearer token with computed refresh / expiry deadlines.
#[derive(Clone)]
pub struct CachedToken {
    access_token: SecretString,
    /// Wall-clock instant the token was received.
    received_at: Instant,
    /// Duration after `received_at` at which the token transitions to `Stale`.
    fresh_until: Duration,
    /// Duration after `received_at` at which the token transitions to `Expired`.
    expires_at: Duration,
}

/// `Debug` redacts the access token to prevent accidental exposure in logs.
impl fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedToken")
            .field("access_token", &"[REDACTED]")
            .field("fresh_until", &self.fresh_until)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl CachedToken {
    /// # Errors
    ///
    /// Returns [`TokenError::InvalidTokenLifetime`] if `lifetime_secs` is zero
    /// or `freshness_ratio` is not in `(0.0, 1.0]`.
    pub(crate) fn new(
        access_token: SecretString,
        lifetime_secs: u64,
        freshness_ratio: f64,
    ) -> Result<Self, super::error::TokenError> {
        if lifetime_secs == 0 {
            return Err(super::error::TokenError::InvalidTokenLifetime(
                "lifetime_secs must be > 0".into(),
            ));
        }
        if !freshness_ratio.is_finite() || freshness_ratio <= 0.0 || freshness_ratio > 1.0 {
            return Err(super::error::TokenError::InvalidTokenLifetime(format!(
                "freshness_ratio must be finite and in (0.0, 1.0], got {freshness_ratio}"
            )));
        }
        let lifetime = Duration::from_secs(lifetime_secs);
        let fresh_until = lifetime.mul_f64(freshness_ratio);
        if fresh_until.is_zero() {
            return Err(super::error::TokenError::InvalidTokenLifetime(format!(
                "freshness window rounds to zero: lifetime_secs={lifetime_secs}, \
                 freshness_ratio={freshness_ratio}"
            )));
        }
        Ok(Self {
            access_token,
            received_at: Instant::now(),
            fresh_until,
            expires_at: lifetime,
        })
    }

    pub(crate) fn access_token(&self) -> &str {
        self.access_token.expose()
    }

    pub(crate) fn token_status(&self) -> TokenStatus {
        let elapsed = self.received_at.elapsed();
        if elapsed >= self.expires_at {
            TokenStatus::Expired
        } else if elapsed >= self.fresh_until {
            TokenStatus::Stale
        } else {
            TokenStatus::Fresh
        }
    }

    /// How long until this token transitions from Fresh → Stale.
    /// Returns `Duration::ZERO` if already stale or expired.
    fn time_until_stale(&self) -> Duration {
        self.fresh_until
            .checked_sub(self.received_at.elapsed())
            .unwrap_or(Duration::ZERO)
    }

    /// How long until this token expires.
    /// Returns `Duration::ZERO` if already expired.
    fn time_until_expired(&self) -> Duration {
        self.expires_at
            .checked_sub(self.received_at.elapsed())
            .unwrap_or(Duration::ZERO)
    }
}

// ---------------------------------------------------------------------------
// Background refresh loop
// ---------------------------------------------------------------------------

/// Configuration for the background refresh watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Maximum random jitter subtracted from the normal pre-stale refresh
    /// delay. Not applied to retry backoff after refresh failures.
    jitter_max: Duration,
    /// Minimum period between consecutive refresh attempts (initial backoff).
    min_refresh_period: Duration,
    /// Backoff multiplier (applied on each consecutive error).
    backoff_multiplier: u32,
    /// Maximum backoff duration.
    max_backoff: Duration,
}

impl WatcherConfig {
    /// # Errors
    ///
    /// Returns [`TokenError::ConfigError`] if `min_refresh_period` is zero.
    pub(crate) fn new(
        jitter_max: Duration,
        min_refresh_period: Duration,
    ) -> Result<Self, super::error::TokenError> {
        if min_refresh_period.is_zero() {
            return Err(super::error::TokenError::ConfigError(
                "min_refresh_period must be > 0".into(),
            ));
        }
        Ok(Self {
            jitter_max,
            min_refresh_period,
            backoff_multiplier: 2,
            max_backoff: min_refresh_period.saturating_mul(30),
        })
    }
}

/// Result of a single token fetch — the source returns this.
#[derive(Debug)]
pub struct FetchedToken {
    pub access_token: SecretString,
    pub lifetime_secs: u64,
    /// Freshness ratio (0.0–1.0): fraction of lifetime during which the token
    /// is considered "fresh". After this fraction, it becomes "stale" and the
    /// watcher attempts a refresh.
    pub freshness_ratio: f64,
}

/// A handle to the background-refreshed token cache.
///
/// Internally stores the latest [`CachedToken`] in an `ArcSwap` for lock-free
/// reads, and drives a `tokio::spawn`ed loop that refreshes the token before
/// it goes stale.
pub struct TokenWatcher {
    current: Arc<ArcSwap<CachedToken>>,
    /// Held so dropping the watcher signals the background task to shut down.
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TokenWatcher {
    /// Perform the initial fetch, cache the token, and spawn the background
    /// refresh loop.
    ///
    /// `source` is moved into the background task and used for all subsequent
    /// refreshes. The initial fetch is performed inline so that startup errors
    /// propagate to the caller.
    ///
    /// # Errors
    ///
    /// Propagates the error from the initial `source.request_token()` call —
    /// the watcher is not created if the first token cannot be obtained.
    pub(crate) async fn spawn(
        mut source: super::source::OAuthTokenSource,
        config: WatcherConfig,
    ) -> Result<Self, super::error::TokenError> {
        // Initial fetch — fail fast if the token endpoint is unreachable.
        let initial = source.request_token().await?;
        let cached = CachedToken::new(
            initial.access_token,
            initial.lifetime_secs,
            initial.freshness_ratio,
        )?;
        let current = Arc::new(ArcSwap::from_pointee(cached));

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let current_for_task = Arc::clone(&current);
        tokio::spawn(async move {
            let mut consecutive_errors: u32 = 0;

            loop {
                let guard = current_for_task.load();
                let base_delay = guard.time_until_stale();
                let until_expired = guard.time_until_expired();

                // Cap jitter to base_delay so it never creates a zero-delay
                // refresh for short-lived tokens.
                let jitter_cap = config.jitter_max.min(base_delay);
                let jitter = random_jitter(jitter_cap);
                let delay = base_delay.saturating_sub(jitter);

                let sleep_dur = if consecutive_errors > 0 {
                    let backoff = compute_backoff(
                        config.min_refresh_period,
                        config.max_backoff,
                        config.backoff_multiplier,
                        consecutive_errors,
                    );
                    // Use backoff directly — but cap to time-until-expired so
                    // we don't sleep past expiry.
                    if until_expired.is_zero() {
                        tracing::warn!(
                            "OAuth2 token watcher: cached token has expired, refresh still failing"
                        );
                        backoff
                    } else {
                        backoff.min(until_expired)
                    }
                } else {
                    delay
                };

                // Drop the ArcSwap guard before sleeping.
                drop(guard);

                tokio::select! {
                    () = tokio::time::sleep(sleep_dur) => {}
                    _ = &mut shutdown_rx => {
                        tracing::debug!("OAuth2 token watcher: shutdown signal received");
                        return;
                    }
                }

                // Attempt refresh — also listen for shutdown so a long
                // request_token() doesn't block drop.
                let refresh_result = tokio::select! {
                    result = source.request_token() => result,
                    _ = &mut shutdown_rx => {
                        tracing::debug!("OAuth2 token watcher: shutdown signal received");
                        return;
                    }
                };

                match refresh_result {
                    Ok(fetched) => {
                        match CachedToken::new(
                            fetched.access_token,
                            fetched.lifetime_secs,
                            fetched.freshness_ratio,
                        ) {
                            Ok(new_cached) => {
                                consecutive_errors = 0;
                                current_for_task.store(Arc::new(new_cached));
                                tracing::debug!(
                                    "OAuth2 token watcher: refreshed token successfully"
                                );
                            }
                            Err(e) => {
                                consecutive_errors = consecutive_errors.saturating_add(1);
                                tracing::warn!(
                                    error = %e,
                                    consecutive_errors,
                                    "OAuth2 token watcher: server returned invalid token"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        tracing::warn!(
                            error = %e,
                            consecutive_errors,
                            "OAuth2 token watcher: refresh failed"
                        );
                    }
                }
            }
        });

        Ok(Self {
            current,
            _shutdown: shutdown_tx,
        })
    }

    /// Read the current cached token (lock-free).
    ///
    /// Returns the token if it is [`Fresh`](TokenStatus::Fresh) or
    /// [`Stale`](TokenStatus::Stale). Returns an error if the token has
    /// [`Expired`](TokenStatus::Expired).
    pub(crate) fn valid_token(
        &self,
    ) -> Result<arc_swap::Guard<Arc<CachedToken>>, super::error::TokenError> {
        let guard = self.current.load();
        if matches!(guard.token_status(), TokenStatus::Expired) {
            return Err(super::error::TokenError::Unavailable(
                "token expired, refresh pending".into(),
            ));
        }
        Ok(guard)
    }
}

/// Compute a random jitter duration in `[0, max)`.
fn random_jitter(max: Duration) -> Duration {
    let max_nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    let nanos = rand::rng().random_range(0..max_nanos);
    Duration::from_nanos(nanos)
}

/// Exponential backoff: `min * multiplier^(errors-1)`, capped at `max`.
fn compute_backoff(min: Duration, max: Duration, multiplier: u32, errors: u32) -> Duration {
    let factor = u64::from(multiplier).saturating_pow(errors.saturating_sub(1));
    let clamped = u32::try_from(factor).unwrap_or(u32::MAX);
    let backoff = min.saturating_mul(clamped);
    backoff.min(max)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // -- CachedToken ----------------------------------------------------------

    #[test]
    fn cached_token_fresh_immediately() {
        let ct = CachedToken::new(SecretString::new("tok"), 3600, 0.8).unwrap();
        assert_eq!(ct.access_token(), "tok");
        assert_eq!(ct.token_status(), TokenStatus::Fresh);
    }

    #[test]
    fn cached_token_zero_lifetime_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 0, 0.8).unwrap_err();
        assert!(
            err.to_string().contains("lifetime_secs"),
            "expected lifetime error, got: {err}"
        );
    }

    #[test]
    fn cached_token_zero_freshness_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 3600, 0.0).unwrap_err();
        assert!(
            err.to_string().contains("freshness_ratio"),
            "expected freshness error, got: {err}"
        );
    }

    #[test]
    fn cached_token_negative_freshness_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 3600, -0.1).unwrap_err();
        assert!(
            err.to_string().contains("freshness_ratio"),
            "expected freshness error, got: {err}"
        );
    }

    #[test]
    fn cached_token_freshness_above_one_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 3600, 1.1).unwrap_err();
        assert!(
            err.to_string().contains("freshness_ratio"),
            "expected freshness error, got: {err}"
        );
    }

    #[test]
    fn cached_token_nan_freshness_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 3600, f64::NAN).unwrap_err();
        assert!(
            err.to_string().contains("freshness_ratio"),
            "expected freshness error, got: {err}"
        );
    }

    #[test]
    fn cached_token_inf_freshness_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 3600, f64::INFINITY).unwrap_err();
        assert!(
            err.to_string().contains("freshness_ratio"),
            "expected freshness error, got: {err}"
        );
    }

    #[test]
    fn cached_token_tiny_freshness_zero_window_rejected() {
        let err = CachedToken::new(SecretString::new("tok"), 1, 0.000_000_000_01).unwrap_err();
        assert!(
            err.to_string().contains("freshness window rounds to zero"),
            "expected zero-window error, got: {err}"
        );
    }

    #[test]
    fn cached_token_full_freshness() {
        // freshness_ratio=1.0 → fresh_until=lifetime → only stale at expiry
        let ct = CachedToken::new(SecretString::new("tok"), 3600, 1.0).unwrap();
        assert_eq!(ct.token_status(), TokenStatus::Fresh);
    }

    #[test]
    fn time_until_stale_positive_when_fresh() {
        let ct = CachedToken::new(SecretString::new("tok"), 3600, 0.8).unwrap();
        assert!(ct.time_until_stale() > Duration::ZERO);
    }

    #[test]
    fn time_until_expired_positive_when_fresh() {
        let ct = CachedToken::new(SecretString::new("tok"), 3600, 0.8).unwrap();
        assert!(ct.time_until_expired() > Duration::ZERO);
    }

    // -- WatcherConfig --------------------------------------------------------

    #[test]
    fn watcher_config_valid() {
        let cfg = WatcherConfig::new(Duration::from_secs(5), Duration::from_secs(1));
        assert!(cfg.is_ok());
    }

    #[test]
    fn watcher_config_zero_min_refresh_rejected() {
        let err = WatcherConfig::new(Duration::from_secs(5), Duration::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("min_refresh_period"),
            "expected min_refresh_period error, got: {err}"
        );
    }

    // -- random_jitter --------------------------------------------------------

    #[test]
    fn jitter_zero_max_returns_zero() {
        assert_eq!(random_jitter(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn jitter_within_bounds() {
        let max = Duration::from_secs(10);
        for _ in 0..100 {
            let j = random_jitter(max);
            assert!(j < max, "jitter {j:?} must be < {max:?}");
        }
    }

    #[test]
    fn jitter_sub_millisecond_does_not_panic() {
        let max = Duration::from_nanos(500);
        for _ in 0..100 {
            let j = random_jitter(max);
            assert!(j < max, "jitter {j:?} must be < {max:?}");
        }
    }

    // -- compute_backoff ------------------------------------------------------

    #[test]
    fn backoff_first_error() {
        let b = compute_backoff(Duration::from_secs(1), Duration::from_mins(1), 2, 1);
        assert_eq!(b, Duration::from_secs(1));
    }

    #[test]
    fn backoff_second_error() {
        let b = compute_backoff(Duration::from_secs(1), Duration::from_mins(1), 2, 2);
        assert_eq!(b, Duration::from_secs(2));
    }

    #[test]
    fn backoff_capped_at_max() {
        let b = compute_backoff(Duration::from_secs(1), Duration::from_secs(30), 2, 100);
        assert_eq!(b, Duration::from_secs(30));
    }

    // -- TokenWatcher::spawn --------------------------------------------------
    //
    // Integration tests for TokenWatcher::spawn live in token.rs (they use
    // httpmock to provide a real OAuthTokenSource). The tests here cover
    // the unit-level helpers; the token.rs tests cover the full spawn +
    // refresh + invalidate lifecycle.
}

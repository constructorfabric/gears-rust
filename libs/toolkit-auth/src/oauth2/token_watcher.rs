//! Background token refresh with jitter and exponential backoff.
//!
//! [`TokenWatcher`] supports eager acquisition and lazy acquisition on first use.
//! One background task retries acquisition and refreshes cached tokens.
//! The latest token is stored in an [`ArcSwap`] for lock-free reads.
//!
//! The public surface is intentionally minimal — only [`CachedToken`],
//! [`TokenWatcher`], and [`TokenStatus`] are used by the rest of the crate.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use arc_swap::ArcSwap;
use tokio::sync::watch;

use super::error::TokenError;
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

    /// Returns the current access token as a string slice (exposes the secret).
    pub(crate) fn access_token(&self) -> &str {
        self.access_token.expose()
    }

    /// Classifies the cached token as fresh, stale or expired based on elapsed time since it was received.
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
#[derive(Debug, Clone, Copy)]
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
    /// The bearer access token returned by the token endpoint.
    pub access_token: SecretString,
    /// Token lifetime in seconds as reported by the token endpoint; must be greater than zero.
    pub lifetime_secs: u64,
    /// Freshness ratio (0.0–1.0): fraction of lifetime during which the token
    /// is considered "fresh". After this fraction, it becomes "stale" and the
    /// watcher attempts a refresh.
    pub freshness_ratio: f64,
}

/// Atomically published token and last acquisition failure.
#[derive(Default)]
struct CacheState {
    token: Option<Arc<CachedToken>>,
    error: Option<TokenError>,
}

/// A separate channel for each attempt keeps results available to slow waiters.
type AttemptResult = Option<Result<Arc<CachedToken>, TokenError>>;

/// A handle to a shared, background-refreshed token cache.
/// Synchronous reads use `ArcSwap::load`; async waiters subscribe to one attempt.
pub struct TokenWatcher {
    current: Arc<ArcSwap<CacheState>>,
    attempt: Arc<ArcSwap<watch::Receiver<AttemptResult>>>,
    start: Arc<tokio::sync::Notify>,
    /// Dropping the watcher cancels sleeps and in-flight requests.
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TokenWatcher {
    /// Fetch the first token inline, then start background refresh.
    ///
    /// # Errors
    /// Propagates initial acquisition and token lifetime validation failures.
    pub(crate) async fn spawn(
        mut source: super::source::OAuthTokenSource,
        config: WatcherConfig,
    ) -> Result<Self, TokenError> {
        let cached = cache_fetched(source.request_token().await?)?;
        Ok(Self::run(source, config, Some(cached)))
    }

    /// Spawn a watcher that waits for first use before acquiring a token.
    pub(crate) fn spawn_lazy(
        source: super::source::OAuthTokenSource,
        config: WatcherConfig,
    ) -> Self {
        Self::run(source, config, None)
    }

    /// Bind the HTTP source to the shared acquisition and refresh loop.
    fn run(
        source: super::source::OAuthTokenSource,
        config: WatcherConfig,
        initial: Option<Arc<CachedToken>>,
    ) -> Self {
        Self::run_with_fetch(
            move || {
                // Clones share the HTTP client and successful discovery result.
                let mut source = source.clone();
                async move { source.request_token().await }
            },
            config,
            initial,
        )
    }

    /// Drive a token source with one retry loop and one result channel per attempt.
    /// The fetch boundary also allows deterministic lifecycle tests without HTTP.
    fn run_with_fetch<F, Fut>(
        mut fetch: F,
        config: WatcherConfig,
        initial: Option<Arc<CachedToken>>,
    ) -> Self
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<FetchedToken, TokenError>> + Send,
    {
        let lazy = initial.is_none();
        let current = Arc::new(ArcSwap::from_pointee(CacheState {
            token: initial,
            error: None,
        }));
        let (mut result_tx, result_rx) = watch::channel(None);
        let attempt = Arc::new(ArcSwap::from_pointee(result_rx));
        let attempt_for_task = attempt.clone();
        let start = Arc::new(tokio::sync::Notify::new());
        let start_for_task = start.clone();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let current_for_task = current.clone();
        tokio::spawn(async move {
            if lazy {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => return,
                    () = start_for_task.notified() => {}
                }
            }
            let mut consecutive_errors = 0_u32;
            loop {
                let sleep_dur =
                    refresh_delay(&current_for_task.load(), &config, consecutive_errors);
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => return,
                    () = tokio::time::sleep(sleep_dur) => {}
                }
                let result = tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => return,
                    result = fetch() => result.and_then(cache_fetched),
                };
                match &result {
                    Ok(token) => {
                        consecutive_errors = 0;
                        current_for_task.store(Arc::new(CacheState {
                            token: Some(token.clone()),
                            error: None,
                        }));
                    }
                    Err(error) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        current_for_task.store(Arc::new(CacheState {
                            token: current_for_task.load().token.clone(),
                            error: Some(error.clone()),
                        }));
                        tracing::warn!(%error, consecutive_errors, "OAuth2 token acquisition failed");
                    }
                }
                // Install the next attempt before waking existing waiters. Their
                // receiver retains this result, even across later completions.
                let (next_tx, next_rx) = watch::channel(None);
                attempt_for_task.store(Arc::new(next_rx));
                result_tx.send_replace(Some(result));
                result_tx = next_tx;
            }
        });
        Self {
            current,
            attempt,
            start,
            _shutdown: shutdown_tx,
        }
    }

    /// Start pending acquisition without bypassing retry backoff.
    pub(crate) fn start_if_pending(&self) -> bool {
        if self.current.load().token.is_none() {
            self.start.notify_one();
            true
        } else {
            false
        }
    }

    /// Read a usable token with an `ArcSwap` guard, notifying only an empty cache.
    /// Failed initial acquisition returns its last error instead of a generic one.
    pub(crate) fn valid_token(&self) -> Result<SecretString, TokenError> {
        let state = self.current.load();
        if let Some(token) = &state.token {
            return usable_secret(token);
        }
        self.start.notify_one();
        Err(state
            .error
            .clone()
            .unwrap_or_else(|| TokenError::Unavailable("initial token acquisition pending".into())))
    }

    /// Subscribe before starting acquisition so a fast completion cannot be missed.
    pub(crate) async fn wait_ready(&self) -> Result<SecretString, TokenError> {
        let mut result_rx = self.attempt.load().as_ref().clone();
        if let Ok(token) = self.valid_token() {
            return Ok(token);
        }
        let outcome = result_rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| TokenError::Unavailable("token watcher stopped".into()))?;
        match outcome.as_ref() {
            Some(Ok(token)) => usable_secret(token),
            Some(Err(error)) => Err(error.clone()),
            None => Err(TokenError::Unavailable("token watcher stopped".into())),
        }
    }
}

/// Validate a fetched token before publishing it to readers and waiters.
fn cache_fetched(fetched: FetchedToken) -> Result<Arc<CachedToken>, TokenError> {
    CachedToken::new(
        fetched.access_token,
        fetched.lifetime_secs,
        fetched.freshness_ratio,
    )
    .map(Arc::new)
}

/// Copy a bearer value only while it remains valid.
fn usable_secret(token: &CachedToken) -> Result<SecretString, TokenError> {
    if token.token_status() == TokenStatus::Expired {
        return Err(TokenError::Unavailable(
            "token expired, refresh pending".into(),
        ));
    }
    Ok(SecretString::new(token.access_token()))
}

/// Schedule initial retries at 1, 2, 4, ... seconds, capped at 30 seconds.
/// Once a token has been obtained, retain the configured refresh policy.
fn refresh_delay(state: &CacheState, config: &WatcherConfig, errors: u32) -> Duration {
    let Some(token) = &state.token else {
        return if errors == 0 {
            Duration::ZERO
        } else {
            compute_backoff(Duration::from_secs(1), Duration::from_secs(30), 2, errors)
        };
    };
    if errors > 0 {
        let backoff = compute_backoff(
            config.min_refresh_period,
            config.max_backoff,
            config.backoff_multiplier,
            errors,
        );
        let until_expired = token.time_until_expired();
        return if until_expired.is_zero() {
            backoff
        } else {
            backoff.min(until_expired)
        };
    }
    let base_delay = token.time_until_stale();
    base_delay.saturating_sub(random_jitter(config.jitter_max.min(base_delay)))
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "token_watcher_lifecycle_tests.rs"]
mod lifecycle_tests;

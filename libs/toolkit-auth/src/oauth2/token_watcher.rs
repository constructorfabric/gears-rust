//! Background token refresh with jitter and exponential backoff.
//!
//! [`TokenWatcher`] starts acquisition lazily on first use.
//! One background task retries acquisition and refreshes cached tokens.
//! The latest token is stored in an `ArcSwapOption` for lock-free reads.
//!
//! The public surface is intentionally minimal — only [`CachedToken`],
//! [`TokenWatcher`], and [`TokenStatus`] are used by the rest of the crate.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

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

/// One acquisition result retained independently for each group of waiters.
type AttemptResult = Option<Result<Arc<CachedToken>, TokenError>>;

/// State serialized between invalidation and publication by the worker.
struct Control {
    attempt: watch::Sender<AttemptResult>,
    refresh_now: bool,
}

/// Shared token worker. The task owns fields, never this handle, so final drop cancels it.
pub struct TokenWatcher {
    current: Arc<ArcSwapOption<CachedToken>>,
    control: Arc<Mutex<Control>>,
    start: Arc<tokio::sync::Notify>,
    wake: watch::Sender<()>,
    cancel: CancellationToken,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for TokenWatcher {
    /// Best-effort cancellation; explicit shutdown also joins the task.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl TokenWatcher {
    /// Create an idle worker using the shared HTTP source and discovery cache.
    pub(crate) fn new(source: super::source::OAuthTokenSource, config: WatcherConfig) -> Self {
        Self::run_with_fetch(
            move || {
                let mut source = source.clone();
                async move { source.request_token().await }
            },
            config,
        )
    }

    /// Run one acquisition task with an injectable source for deterministic tests.
    fn run_with_fetch<F, Fut>(mut fetch: F, config: WatcherConfig) -> Self
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<FetchedToken, TokenError>> + Send,
    {
        let current = Arc::new(ArcSwapOption::empty());
        let (attempt, _) = watch::channel(None);
        let control = Arc::new(Mutex::new(Control {
            attempt,
            refresh_now: false,
        }));
        let start = Arc::new(tokio::sync::Notify::new());
        let (wake, mut wake_rx) = watch::channel(());
        let cancel = CancellationToken::new();
        let task_cache = current.clone();
        let task_control = control.clone();
        let task_start = start.clone();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            // Also wakes waiters if the worker exits unexpectedly.
            let _cancel_on_exit = task_cancel.clone().drop_guard();
            tokio::select! {
                biased;
                () = task_cancel.cancelled() => return,
                () = task_start.notified() => {}
            }
            let mut errors = 0_u32;
            loop {
                let delay = {
                    let mut state = task_control.lock();
                    wake_rx.borrow_and_update();
                    if std::mem::take(&mut state.refresh_now) {
                        errors = 0;
                        Duration::ZERO
                    } else {
                        refresh_delay(task_cache.load().as_deref(), &config, errors)
                    }
                };
                tokio::select! {
                    biased;
                    () = task_cancel.cancelled() => return,
                    _ = wake_rx.changed() => {},
                    () = tokio::time::sleep(delay) => {}
                }
                {
                    let mut state = task_control.lock();
                    if std::mem::take(&mut state.refresh_now) {
                        errors = 0;
                    }
                    wake_rx.borrow_and_update();
                }
                let result = tokio::select! {
                    biased;
                    () = task_cancel.cancelled() => return,
                    result = fetch() => result.and_then(cache_fetched),
                };
                let mut state = task_control.lock();
                // Invalidation during a fetch joins that fetch. Do not let its
                // wakeup trigger a second exchange after this one completes.
                state.refresh_now = false;
                wake_rx.borrow_and_update();
                match &result {
                    Ok(token) => {
                        errors = 0;
                        task_cache.store(Some(token.clone()));
                    }
                    Err(error) => {
                        errors = errors.saturating_add(1);
                        tracing::warn!(%error, errors, "OAuth2 token acquisition failed");
                    }
                }
                let (next, _) = watch::channel(None);
                let finished = std::mem::replace(&mut state.attempt, next);
                finished.send_replace(Some(result));
            }
        });
        Self {
            current,
            control,
            start,
            wake,
            cancel,
            task: tokio::sync::Mutex::new(Some(task)),
        }
    }

    /// Return a usable cache entry or await one shared attempt, respecting backoff.
    pub(crate) async fn get(&self) -> Result<SecretString, TokenError> {
        if self.cancel.is_cancelled() {
            return Err(TokenError::Shutdown);
        }
        if let Some(token) = self.current.load().as_ref()
            && token.token_status() != TokenStatus::Expired
        {
            return Ok(SecretString::new(token.access_token()));
        }
        let receiver = {
            let state = self.control.lock();
            // A fetch may have finished while this caller took the lock.
            if let Some(token) = self.current.load().as_ref()
                && token.token_status() != TokenStatus::Expired
            {
                return Ok(SecretString::new(token.access_token()));
            }
            state.attempt.subscribe()
        };
        self.start.notify_one();
        self.wait_attempt(receiver).await
    }

    /// Discard the cached credential and share the current or next acquisition.
    /// An empty cache means invalidation/acquisition is already pending; its
    /// backoff is preserved. Failed replacement never restores the discarded token.
    pub(crate) async fn invalidate(&self) -> Result<(), TokenError> {
        if self.cancel.is_cancelled() {
            return Err(TokenError::Shutdown);
        }
        let receiver = {
            let mut state = self.control.lock();
            if self.current.load().is_some() {
                self.current.store(None);
                state.refresh_now = true;
                self.wake.send_replace(());
            }
            state.attempt.subscribe()
        };
        self.start.notify_one();
        self.wait_attempt(receiver).await.map(|_| ())
    }

    /// Await exactly one attempt, prioritizing shutdown over cached results.
    async fn wait_attempt(
        &self,
        mut receiver: watch::Receiver<AttemptResult>,
    ) -> Result<SecretString, TokenError> {
        loop {
            let outcome = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(TokenError::Shutdown),
                outcome = receiver.wait_for(Option::is_some) => outcome.map_err(|_| TokenError::Shutdown)?.clone(),
            };
            match outcome {
                Some(Ok(_)) => {
                    let state = self.control.lock();
                    // A completed success may have been invalidated while this
                    // waiter was asleep. Only deliver a currently usable token.
                    if let Some(token) = self.current.load().as_ref()
                        && token.token_status() != TokenStatus::Expired
                    {
                        return Ok(SecretString::new(token.access_token()));
                    }
                    receiver = state.attempt.subscribe();
                }
                Some(Err(error)) => return Err(error),
                None => return Err(TokenError::Shutdown),
            }
        }
    }

    /// Stop all clones and wait for the task to exit. Joining is cancellation-safe:
    /// the handle stays in the mutex if this particular shutdown future is dropped.
    pub(crate) async fn shutdown(&self) {
        self.cancel.cancel();
        let mut task = self.task.lock().await;
        if let Some(handle) = task.as_mut() {
            if let Err(error) = handle.await {
                tracing::error!(%error, "OAuth2 token worker failed during shutdown");
            }
            *task = None;
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

/// Use bounded acquisition retries without a usable token, otherwise the refresh policy.
fn refresh_delay(token: Option<&CachedToken>, config: &WatcherConfig, errors: u32) -> Duration {
    let Some(token) = token.filter(|token| token.token_status() != TokenStatus::Expired) else {
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
        return backoff.min(until_expired);
    }
    let delay = token.time_until_stale();
    delay.saturating_sub(random_jitter(config.jitter_max.min(delay)))
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

    // -- TokenWatcher::new --------------------------------------------------
    //
    // Integration tests for TokenWatcher::new live in token.rs (they use
    // httpmock to provide a real OAuthTokenSource). The tests here cover
    // the unit-level helpers; the token.rs tests cover the full acquisition +
    // refresh + invalidate lifecycle.
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "token_watcher_lifecycle_tests.rs"]
mod lifecycle_tests;

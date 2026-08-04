//! Client and retry configuration consumed by generated REST clients.

use std::time::Duration;

/// Base configuration for a generated REST client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Base URL prefix (e.g., `https://billing.internal`).
    /// Combined with the base path declared in the projection trait.
    pub base_url: String,
    /// Deadline applied to a **single** unary attempt — NOT to the whole logical
    /// call. A `#[retryable]` method may make up to `retry.max_attempts` attempts,
    /// so the worst-case wall-clock for a logical call is bounded by
    /// `max_attempts × (timeout + retry.max_delay)` (the per-retry backoff is
    /// itself clamped to [`RetryConfig::max_delay`], including a server-advised
    /// `Retry-After`). There is deliberately no separate whole-call budget field.
    pub timeout: Duration,
    /// Per-**event** idle deadline for SSE streams: the maximum gap between two
    /// received stream events before the stream is treated as timed out. A
    /// long-lived stream is NOT bounded by [`timeout`](Self::timeout) (which
    /// would kill a healthy slow stream); it is bounded by this larger idle
    /// deadline instead. Defaults to 60s (> the unary default).
    pub sse_idle_timeout: Duration,
    /// Retry policy applied to methods marked `#[retryable]`.
    pub retry: RetryConfig,
    /// SSE-stream reconnect policy. By default `max_attempts: 0` — stream
    /// failures bubble up unchanged. Set explicitly to opt into HTML5
    /// EventSource-style `Last-Event-ID` reconnect.
    pub sse_reconnect: ReconnectConfig,
    /// When `true`, the generated client refuses plaintext `http://` and
    /// requires TLS (`toolkit_http::TransportSecurity::TlsOnly`) for every
    /// request — including the bearer-carrying `Authorization` header, which
    /// otherwise would ride whatever scheme `base_url` uses. Defaults to
    /// `false`, preserving the platform's existing in-mesh
    /// service-to-service convention where plaintext HTTP inside a secured
    /// network boundary is an accepted, deliberate choice (see
    /// [`build_default_http_client`](crate::runtime::client::build_default_http_client)).
    /// Set this when a resolved endpoint may cross an untrusted network.
    pub require_tls: bool,
}

impl ClientConfig {
    /// Create a new config with sensible defaults.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: Duration::from_secs(30),
            sse_idle_timeout: Duration::from_mins(1),
            retry: RetryConfig::default(),
            sse_reconnect: ReconnectConfig::default(),
            require_tls: false,
        }
    }

    /// Override the per-call (unary) timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the SSE per-event idle deadline (max gap between stream events).
    #[must_use]
    pub fn with_sse_idle_timeout(mut self, idle: Duration) -> Self {
        self.sse_idle_timeout = idle;
        self
    }

    /// Override the retry policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Override the SSE reconnect policy. Use [`ReconnectConfig::default()`]
    /// to disable (the default) or build a non-zero `max_attempts` policy
    /// to enable reconnect.
    #[must_use]
    pub fn with_sse_reconnect(mut self, sse_reconnect: ReconnectConfig) -> Self {
        self.sse_reconnect = sse_reconnect;
        self
    }

    /// Require TLS (reject plaintext `http://`) for this client. See
    /// [`Self::require_tls`].
    #[must_use]
    pub fn with_require_tls(mut self, require_tls: bool) -> Self {
        self.require_tls = require_tls;
        self
    }
}

/// Bounded exponential-backoff retry policy with full jitter.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (must be at least 1).
    pub max_attempts: u32,
    /// Base delay before the first retry.
    pub base_delay: Duration,
    /// Hard cap on the delay between retries.
    pub max_delay: Duration,
    /// Multiplier applied between consecutive retries.
    pub multiplier: f64,
}

impl RetryConfig {
    /// Disable retries entirely (single attempt).
    #[must_use]
    pub const fn off() -> Self {
        Self {
            max_attempts: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            multiplier: 1.0,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: 2.0,
        }
    }
}

/// SSE reconnect policy. The streaming client tracks the latest `id:`
/// field seen on the wire and, on transient stream failures, re-issues
/// the request with a `Last-Event-ID: <stored>` header so the server can
/// resume the event sequence (per HTML5 `EventSource` spec).
///
/// Default is **opt-in disabled** (`max_attempts: 0`) so existing SDKs see
/// no behaviour change.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Maximum number of reconnect attempts after the initial connection.
    /// `0` (default) disables reconnect entirely — stream errors bubble up.
    pub max_attempts: u32,
    /// Initial delay before the first reconnect attempt.
    pub base_delay: Duration,
    /// Hard cap on delay between reconnect attempts.
    pub max_delay: Duration,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_attempts: 0,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl ReconnectConfig {
    /// Build a reconnect policy with up to `max_attempts` retries and the
    /// supplied initial delay (capped by `max_delay`, default 10s).
    #[must_use]
    pub fn enabled(max_attempts: u32, base_delay: Duration) -> Self {
        Self {
            max_attempts,
            base_delay,
            max_delay: Duration::from_secs(10),
        }
    }

    /// Override the maximum delay between reconnect attempts.
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn default_retry_has_three_attempts() {
        let r = RetryConfig::default();
        assert_eq!(r.max_attempts, 3);
        assert!(r.base_delay > Duration::ZERO);
    }

    #[test]
    fn off_yields_single_attempt() {
        let r = RetryConfig::off();
        assert_eq!(r.max_attempts, 1);
    }

    #[test]
    fn client_config_chains_overrides() {
        let cfg = ClientConfig::new("https://x.example")
            .with_timeout(Duration::from_secs(5))
            .with_retry(RetryConfig::off());
        assert_eq!(cfg.base_url, "https://x.example");
        assert_eq!(cfg.timeout, Duration::from_secs(5));
        assert_eq!(cfg.retry.max_attempts, 1);
    }
}

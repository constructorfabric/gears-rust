//! Client, retry, and credential configuration consumed by generated clients.
//!
//! [`ClientConfig`] is shared by both the generated REST client and the
//! generated gRPC client; [`InternalTokenProvider`] is the runtime source of the
//! platform-plane credential those clients attach on `PlatformSecurityContext`
//! methods.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

/// Outcome of resolving the process's platform-plane credential on one outbound
/// call.
///
/// Three-state (rather than `Option<SecretString>`) so an attach site can tell
/// an intentionally unauthenticated deployment (Profile 1) apart from a broken
/// credential source — e.g. the projected token file is transiently empty.
/// Attach helpers stay silent on [`Self::NotConfigured`] and `warn!` on
/// [`Self::Unavailable`], never emitting the token.
#[derive(Debug)]
pub enum CredentialState {
    /// No credential configured (Profile 1 / `InternalCredential::None`). Attach
    /// nothing, silently — a legitimate deployment.
    NotConfigured,
    /// A credential is configured and currently available; attach it.
    Available(SecretString),
    /// Configured but currently unavailable (empty token file, or the background
    /// refresh has not run yet). Attach nothing but **warn** — a broken source,
    /// not an intentional opt-out. Carries the reason (never the token).
    Unavailable(Cow<'static, str>),
}

/// Source of the process's **platform-plane** internal credential.
///
/// Generated clients attach it — as the `X-ToolKit-Internal-Token` header /
/// metadata, **never** `Authorization` — on methods whose plane marker is
/// `PlatformSecurityContext` (`cpt-cf-adr-two-plane-auth`). The credential comes
/// from the runtime (the bootstrap-selected `InternalCredential`), never the
/// contract argument.
///
/// Invoked on every call so a rotating credential (e.g. a projected Kubernetes
/// `ServiceAccount` token) is always attached in its current form; it returns a
/// [`CredentialState`] to distinguish not-configured (silent) from unavailable
/// (warn). Because it runs per-request on an async path, the closure **must not
/// block, do I/O, or take a contended lock** (see [`InternalTokenProvider::new`]).
#[derive(Clone)]
pub struct InternalTokenProvider(Arc<dyn Fn() -> CredentialState + Send + Sync>);

impl InternalTokenProvider {
    /// Build a provider whose credential is resolved by `provider` on each call
    /// (supports rotation).
    ///
    /// The closure **must not block, do I/O, or take a contended lock** — it is
    /// called on every outbound platform-plane request from an async path. See
    /// [`InternalTokenProvider`] and use `ServiceAccountTokenReader::token_provider`
    /// as the reference pattern for a rotating credential.
    #[must_use]
    pub fn new(provider: impl Fn() -> CredentialState + Send + Sync + 'static) -> Self {
        Self(Arc::new(provider))
    }

    /// Build a provider that always yields the given static `token`.
    ///
    /// Suitable for a non-rotating credential (e.g. a shared secret); prefer
    /// [`InternalTokenProvider::new`] for rotating tokens.
    #[must_use]
    pub fn from_token(token: SecretString) -> Self {
        Self::new(move || CredentialState::Available(token.clone()))
    }

    /// Resolve the current credential state.
    #[must_use]
    pub fn current(&self) -> CredentialState {
        (self.0)()
    }

    /// Resolve the token to attach on an outbound platform-plane call, applying
    /// the shared attach policy so the REST and gRPC helpers behave identically:
    /// `None`/[`NotConfigured`](CredentialState::NotConfigured) → `None` (silent),
    /// [`Available`](CredentialState::Available) → `Some`, and
    /// [`Unavailable`](CredentialState::Unavailable) → `None` plus a `warn!`
    /// naming the plane and `rpc` (never the token).
    #[must_use]
    pub fn resolve_for_attach(provider: Option<&Self>, rpc: &str) -> Option<SecretString> {
        match provider.map(Self::current) {
            None | Some(CredentialState::NotConfigured) => None,
            Some(CredentialState::Available(token)) => Some(token),
            Some(CredentialState::Unavailable(reason)) => {
                tracing::warn!(
                    plane = "platform",
                    rpc,
                    reason = %reason,
                    "platform-plane credential configured but currently unavailable; \
                     sending request without the internal token",
                );
                None
            }
        }
    }
}

impl std::fmt::Debug for InternalTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the credential (or even hint at its presence beyond the
        // opaque marker) so it cannot leak through a `{:?}` sink.
        f.write_str("InternalTokenProvider(<fn>)")
    }
}

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
    /// Source of the platform-plane internal credential attached to methods
    /// whose plane marker is `PlatformSecurityContext` (carried as
    /// `X-ToolKit-Internal-Token`). `None` (the default) attaches nothing —
    /// legitimate for Profile 1 / in-process (`InternalCredential::None`);
    /// the requirement is enforced server-side. The bootstrap layer populates
    /// this from the process's selected `InternalCredential`. Tenant-plane
    /// methods (`SecurityContext`) never consult it; they forward the caller's
    /// bearer token from the argument.
    pub internal_token_provider: Option<InternalTokenProvider>,
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
            internal_token_provider: None,
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

    /// Set (or clear) the platform-plane internal-credential provider. See
    /// [`Self::internal_token_provider`]. Accepts either an
    /// [`InternalTokenProvider`] or an `Option<InternalTokenProvider>`, so the
    /// bootstrap layer can pass through whatever the process selected without a
    /// branch.
    #[must_use]
    pub fn with_internal_token_provider(
        mut self,
        provider: impl Into<Option<InternalTokenProvider>>,
    ) -> Self {
        self.internal_token_provider = provider.into();
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

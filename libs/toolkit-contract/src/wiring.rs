//! `ClientWiring` — typed config schema consumed by `#[toolkit::provides]`.
//!
//! Lives outside the feature-gated `runtime` module so the deserialization
//! itself is always available: any module loaded into the host must be able
//! to parse its wiring config regardless of which transport features its
//! provider SDK compiled in. The actual conversion to a runtime
//! [`ClientConfig`](crate::runtime::config::ClientConfig) is gated on
//! `runtime-client`.

use std::time::Duration;

use serde::Deserialize;

/// Fine-tuning knobs forwarded to the transport client when a remote
/// transport is selected. All fields are optional — missing values fall
/// back to the SDK defaults baked into
/// [`ClientConfig`](crate::runtime::config::ClientConfig).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClientTuning {
    /// Per-call request deadline (e.g., `"5s"`, `"500ms"`).
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub timeout: Option<Duration>,

    /// Override for the retry policy applied to `#[retryable]` methods.
    #[serde(default)]
    pub retry: Option<RetrySettings>,

    /// Override for the stream reconnect policy, of any framing.
    ///
    /// `alias = "sse_reconnect"` keeps already-deployed config files working:
    /// this struct has no `rename_all`, so the Rust field name *is* the JSON
    /// key, and the field was called `sse_reconnect` before the policy covered
    /// framings other than SSE.
    ///
    /// Supplying **both** `stream_reconnect` and `sse_reconnect` is rejected as
    /// a duplicate field — the two name the same field — so a config that adds
    /// the new key without removing the legacy one fails loudly at parse time
    /// rather than silently honouring one and dropping the other. This holds
    /// even though the field is `#[serde(flatten)]`-ed into [`ClientWiring`].
    #[serde(default, alias = "sse_reconnect")]
    pub stream_reconnect: Option<ReconnectSettings>,

    /// Reject plaintext `http://` endpoints.
    ///
    /// Defaults to `false` (the in-mesh convention). Without this knob a
    /// discovery-resolved client always got the default, so a consumer talking
    /// to an endpoint outside a trusted boundary had no way to demand TLS —
    /// while forwarding a tenant bearer token over it.
    #[serde(default)]
    pub require_tls: Option<bool>,

    /// Circuit breaker for each endpoint this client calls. Missing keeps the
    /// defaults (on: opens when at least half of at least 10 calls in 30s
    /// failed at the transport level, open for 10s, then one probe);
    /// `{ enabled: false }` turns it off. Unknown keys and invalid values are
    /// rejected at parse time.
    #[cfg(feature = "runtime-client")]
    #[serde(default)]
    pub circuit_breaker: Option<toolkit_resilience::circuit_breaker::CircuitBreakerConfig>,

    /// Max idle keep-alive connections per upstream host. Raise this to at least
    /// the expected per-upstream request concurrency so HTTP/1.1 connections are
    /// reused instead of churned under load. Missing keeps the SDK default
    /// ([`ClientConfig::pool_max_idle_per_host`](crate::runtime::config::ClientConfig::pool_max_idle_per_host)).
    ///
    /// **REST transport only.** A gRPC client accepts this key but it has no
    /// effect (a warning is logged when such a client is wired).
    #[serde(default)]
    pub pool_max_idle_per_host: Option<usize>,

    /// How long idle keep-alive connections are retained (e.g. `"90s"`, `"2m"`)
    /// before being closed and reopened on the next request. Companion of
    /// `pool_max_idle_per_host`; raise it above the gap between successive bursts
    /// to a given upstream to keep connections warm. Missing keeps the SDK
    /// default ([`ClientConfig::pool_idle_timeout`](crate::runtime::config::ClientConfig::pool_idle_timeout)).
    ///
    /// **REST transport only** (see `pool_max_idle_per_host`).
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub pool_idle_timeout: Option<Duration>,

    /// Max concurrent in-flight requests through this client at once. Requests
    /// beyond the cap are shed immediately (`HttpError::Overloaded`). Keep it at
    /// or above `pool_max_idle_per_host` so the pool can be fully reused. Missing
    /// keeps the SDK default
    /// ([`ClientConfig::max_concurrent_requests`](crate::runtime::config::ClientConfig::max_concurrent_requests)).
    ///
    /// **REST transport only** (see `pool_max_idle_per_host`).
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,

    /// Platform-plane credential source forwarded onto the built
    /// [`ClientConfig`](crate::runtime::config::ClientConfig). Injected by the
    /// runtime's proxy-wiring phase, never from config (`#[serde(skip)]`); gated
    /// on `runtime-client` since the type lives there.
    #[cfg(feature = "runtime-client")]
    #[serde(skip)]
    pub internal_token_provider: Option<crate::runtime::config::InternalTokenProvider>,
}

impl ClientTuning {
    /// Every key a [`ClientTuning`] reads. It is flattened into
    /// [`ConsumerWiring`] and [`ClientWiring`], so serde cannot reject unknown
    /// keys there; the config reader checks against this list instead.
    pub const KEYS: &'static [&'static str] = &[
        "timeout",
        "retry",
        "stream_reconnect",
        "sse_reconnect",
        "require_tls",
        "circuit_breaker",
        "pool_max_idle_per_host",
        "pool_idle_timeout",
        "max_concurrent_requests",
    ];

    /// The knobs set on this tuning that a gRPC client does not apply.
    ///
    /// Used to warn when a gRPC client is wired: it reads the endpoint,
    /// `timeout`, `retry`, `require_tls` and `circuit_breaker`. The pool and
    /// concurrency knobs are REST-only, and gRPC streams do not reconnect yet,
    /// so `stream_reconnect` (or `sse_reconnect`) is not applied either.
    /// Returns their names (empty when none are set).
    #[must_use]
    pub fn grpc_ignored_knobs(&self) -> Vec<&'static str> {
        let mut set = Vec::new();
        if self.stream_reconnect.is_some() {
            set.push("stream_reconnect");
        }
        if self.pool_max_idle_per_host.is_some() {
            set.push("pool_max_idle_per_host");
        }
        if self.pool_idle_timeout.is_some() {
            set.push("pool_idle_timeout");
        }
        if self.max_concurrent_requests.is_some() {
            set.push("max_concurrent_requests");
        }
        set
    }
}

/// Deserializable mirror of
/// [`RetryConfig`](crate::runtime::config::RetryConfig). All fields optional;
/// missing values keep the runtime default.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySettings {
    pub max_attempts: Option<u32>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub base_delay: Option<Duration>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub max_delay: Option<Duration>,
    pub multiplier: Option<f64>,
}

/// Deserializable mirror of
/// [`ReconnectConfig`](crate::runtime::config::ReconnectConfig).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconnectSettings {
    pub max_attempts: Option<u32>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub base_delay: Option<Duration>,
    #[serde(default, with = "toolkit_utils::humantime_serde::option")]
    pub max_delay: Option<Duration>,
}

/// Transport choice + endpoint + tuning for one provided contract.
///
/// Read by `#[toolkit::provides]` from
/// `gears.<gear>.clients.provides.<contract_snake>`, with
/// `gears.<gear>.clients.defaults` merged under it. If the key is absent the
/// wiring defaults to [`ClientWiring::Local`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase", tag = "transport")]
pub enum ClientWiring {
    /// In-process. The provider gear's local factory is invoked.
    #[default]
    Local,
    /// Generated REST client points at `endpoint`.
    Rest {
        endpoint: String,
        #[serde(default, flatten)]
        tuning: ClientTuning,
    },
    /// Generated gRPC client connects to `endpoint`.
    Grpc {
        endpoint: String,
        #[serde(default, flatten)]
        tuning: ClientTuning,
    },
}

/// Whether `s` is an absolute URI (scheme + authority) — the shape a usable
/// static endpoint override must have. A bare host, relative path or empty string
/// would otherwise be marked resolved with no probe (`/readyz` 200 while every
/// call fails). Enforced at parse time by [`ConsumerWiring`]'s `endpoint` deserializer.
#[must_use]
fn is_absolute_endpoint(s: &str) -> bool {
    s.parse::<http::Uri>()
        .is_ok_and(|uri| uri.scheme().is_some() && uri.authority().is_some())
}

/// `deserialize_with` for [`ConsumerWiring::endpoint`]: keeps the field a plain
/// `Option<String>` while rejecting a present, non-absolute value at parse time
/// (see [`is_absolute_endpoint`]).
fn deserialize_optional_endpoint<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let endpoint = Option::<String>::deserialize(deserializer)?;
    if let Some(ref s) = endpoint
        && !is_absolute_endpoint(s)
    {
        return Err(serde::de::Error::custom(format!(
            "`{s}` is not an absolute URI (needs scheme + host, e.g. `http://host:port`)"
        )));
    }
    Ok(endpoint)
}

/// Consumer-side wiring for one dependency, read from
/// `gears.<owner>.clients.consumes.<dep>` (with `gears.<owner>.clients.defaults`
/// merged under it) by the runtime's proxy-wiring phase and applied to the
/// directory-resolving client that `#[toolkit::consumes]` registers.
///
/// An object with an optional `endpoint` (omit to keep discovery) plus a
/// flattened [`ClientTuning`]:
///
/// ```yaml
/// clients:
///   consumes:
///     api-contracts:
///       timeout: "5s"
///       max_concurrent_requests: 256
///       # endpoint: "http://..."        # optional; omit to keep discovery
///       # grpc_endpoint: "http://..."   # optional; for gRPC clients of this dep
/// ```
///
/// Unlike [`ClientWiring`] there is **no** `transport` tag: the transport is
/// chosen in code (`#[toolkit::consumes(.., transport = grpc)]`). A gRPC client
/// uses `grpc_endpoint` when set, else `endpoint`; it ignores the REST-only
/// pool and concurrency knobs and `stream_reconnect` (see
/// [`ClientTuning::grpc_ignored_knobs`]).
///
/// Must be an object; a bare `<dep>: "http://host"` string (the old shape) is
/// rejected at parse time — use the `endpoint` field.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConsumerWiring {
    /// Static-endpoint override (ADR-0004 dev/test escape hatch): when set, the
    /// dep is wired to this fixed endpoint instead of being discovered. Rejected
    /// at parse time unless it is an absolute URI (see [`is_absolute_endpoint`]).
    #[serde(default, deserialize_with = "deserialize_optional_endpoint")]
    pub endpoint: Option<String>,
    /// Static-endpoint override for gRPC clients of this dep, when they must
    /// not use `endpoint` (a gear that consumes the dep over both REST and
    /// gRPC). Validated like `endpoint`.
    #[serde(default, deserialize_with = "deserialize_optional_endpoint")]
    pub grpc_endpoint: Option<String>,
    /// Client tuning, flattened into the same map as `endpoint`.
    #[serde(default, flatten)]
    pub tuning: ClientTuning,
}

impl ConsumerWiring {
    /// Split the wiring into its static-endpoint override (if any) and the
    /// [`ClientTuning`] to apply.
    #[must_use]
    pub fn into_parts(self) -> (Option<String>, ClientTuning) {
        (self.endpoint, self.tuning)
    }
}

#[cfg(feature = "runtime-client")]
impl ClientTuning {
    /// Apply tuning overrides onto a fresh [`ClientConfig`] built from `endpoint`.
    #[must_use]
    pub fn apply_to(&self, endpoint: impl Into<String>) -> crate::runtime::config::ClientConfig {
        use crate::runtime::config::{ClientConfig, ReconnectConfig, RetryConfig};

        let mut cfg = ClientConfig::new(endpoint);
        if let Some(timeout) = self.timeout {
            cfg = cfg.with_timeout(timeout);
        }
        if let Some(ref r) = self.retry {
            let base = cfg.retry.clone();
            cfg = cfg.with_retry(RetryConfig {
                max_attempts: r.max_attempts.unwrap_or(base.max_attempts),
                base_delay: r.base_delay.unwrap_or(base.base_delay),
                max_delay: r.max_delay.unwrap_or(base.max_delay),
                multiplier: r.multiplier.unwrap_or(base.multiplier),
            });
        }
        if let Some(ref s) = self.stream_reconnect {
            let base = cfg.stream_reconnect.clone();
            cfg = cfg.with_stream_reconnect(ReconnectConfig {
                max_attempts: s.max_attempts.unwrap_or(base.max_attempts),
                base_delay: s.base_delay.unwrap_or(base.base_delay),
                max_delay: s.max_delay.unwrap_or(base.max_delay),
                // Not exposed as wiring keys yet; inherit the base policy.
                min_healthy_uptime: base.min_healthy_uptime,
                max_total_reopens: base.max_total_reopens,
            });
        }
        if let Some(require_tls) = self.require_tls {
            cfg = cfg.with_require_tls(require_tls);
        }
        if let Some(max) = self.pool_max_idle_per_host {
            cfg = cfg.with_pool_max_idle_per_host(max);
        }
        if let Some(timeout) = self.pool_idle_timeout {
            cfg = cfg.with_pool_idle_timeout(Some(timeout));
        }
        if let Some(max) = self.max_concurrent_requests {
            cfg = cfg.with_max_concurrent_requests(Some(max));
        }
        cfg = cfg.with_internal_token_provider(self.internal_token_provider.clone());
        cfg
    }

    /// The circuit breaker this tuning asks for, named `name` in logs and
    /// rejections; `None` when it is turned off.
    #[must_use]
    pub fn circuit_breaker_for(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<toolkit_resilience::circuit_breaker::CircuitBreaker>> {
        use toolkit_resilience::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};

        let config = self.circuit_breaker.clone().unwrap_or_default();
        if !config.enabled() {
            return None;
        }
        // Parsed configs are validated already; a hand-built invalid one runs
        // with the defaults rather than without a breaker.
        let breaker = CircuitBreaker::new(config).or_else(|err| {
            tracing::warn!(breaker = name, error = %err, "invalid circuit breaker config; using defaults");
            CircuitBreaker::new(CircuitBreakerConfig::default())
        });
        breaker.ok().map(|b| std::sync::Arc::new(b.with_name(name)))
    }

    /// Attach the platform-plane credential source forwarded onto the built
    /// [`ClientConfig`](crate::runtime::config::ClientConfig). Used by the
    /// proxy-wiring phase to thread the process credential into a
    /// directory-resolving (`#[toolkit::consumes]`) client.
    #[must_use]
    pub fn with_internal_token_provider(
        mut self,
        provider: Option<crate::runtime::config::InternalTokenProvider>,
    ) -> Self {
        self.internal_token_provider = provider;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A static override is only usable if it is an absolute URI. An empty string,
    /// a bare host, or a relative path would otherwise reach the wiring code, take
    /// the static branch, and mark the dep resolved with no probe — 200 on
    /// `/readyz` while every call fails.
    #[test]
    fn is_absolute_endpoint_accepts_only_scheme_plus_authority() {
        assert!(is_absolute_endpoint("http://host:8081"));
        assert!(is_absolute_endpoint("https://billing.svc/base"));
        assert!(!is_absolute_endpoint(""));
        assert!(!is_absolute_endpoint("billing"));
        assert!(!is_absolute_endpoint("/path/only"));
        assert!(!is_absolute_endpoint("not a uri"));
    }

    /// `ConsumerWiring` validates the endpoint at parse time: a present-but-invalid
    /// value fails to deserialize (rather than yielding an unchecked string), the
    /// valid value survives verbatim, and an omitted `endpoint` keeps discovery
    /// while still parsing the tuning.
    #[test]
    fn consumer_wiring_validates_endpoint_on_deserialize() {
        let ok: ConsumerWiring = serde_json::from_value(serde_json::json!({
            "endpoint": "http://billing:8080",
            "max_concurrent_requests": 256
        }))
        .expect("valid absolute endpoint must parse");
        let (endpoint, tuning) = ok.into_parts();
        assert_eq!(endpoint.as_deref(), Some("http://billing:8080"));
        assert_eq!(tuning.max_concurrent_requests, Some(256));

        assert!(
            serde_json::from_value::<ConsumerWiring>(serde_json::json!({ "endpoint": "" }))
                .is_err(),
            "an empty endpoint must be rejected at deserialize time"
        );
        assert!(
            serde_json::from_value::<ConsumerWiring>(serde_json::json!({ "endpoint": "billing" }))
                .is_err(),
            "a bare host must be rejected at deserialize time"
        );

        let no_endpoint: ConsumerWiring =
            serde_json::from_value(serde_json::json!({ "max_concurrent_requests": 1 }))
                .expect("omitted endpoint keeps discovery");
        assert_eq!(no_endpoint.endpoint, None);
        assert_eq!(no_endpoint.tuning.max_concurrent_requests, Some(1));
    }
}

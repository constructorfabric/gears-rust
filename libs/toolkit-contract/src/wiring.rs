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

    /// Max idle keep-alive connections per upstream host. Raise this to at least
    /// the expected per-upstream request concurrency so HTTP/1.1 connections are
    /// reused instead of churned under load. Missing keeps the SDK default
    /// ([`ClientConfig::pool_max_idle_per_host`](crate::runtime::config::ClientConfig::pool_max_idle_per_host)).
    ///
    /// **REST transport only.** A `transport: grpc` wiring accepts this key but
    /// it has no effect — the gRPC client reads only `endpoint`, `timeout` and
    /// `require_tls` (a warning is logged at gRPC client construction).
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
    /// REST-only transport knobs that were explicitly set on this tuning.
    ///
    /// Used to warn when a non-REST transport is selected: the gRPC client reads
    /// only `endpoint`, `timeout` and `require_tls`, so
    /// `pool_max_idle_per_host`, `pool_idle_timeout` and
    /// `max_concurrent_requests` on a `transport: grpc` wiring silently go
    /// nowhere. Returns their names (empty when none are set).
    #[must_use]
    pub fn rest_only_knobs_set(&self) -> Vec<&'static str> {
        let mut set = Vec::new();
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
/// `gears.<gear>.config.client_wiring.<contract_snake>`. If the key is
/// absent the wiring defaults to [`ClientWiring::Local`].
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

/// Consumer-side wiring for one dependency, read from
/// `gears.<owner>.config.consumer_wiring.<dep>` by the runtime's proxy-wiring
/// phase and applied to the directory-resolving REST client that
/// `#[toolkit::consumes]` registers.
///
/// An object with an optional `endpoint` (omit to keep discovery) plus a
/// flattened [`ClientTuning`]:
///
/// ```yaml
/// consumer_wiring:
///   api-contracts:
///     timeout: "5s"
///     max_concurrent_requests: 256
///     # endpoint: "http://..."   # optional; omit to keep discovery
/// ```
///
/// Unlike [`ClientWiring`] there is **no** `transport` tag: the consumer path is
/// REST-only (`#[toolkit::consumes]` always wires a `<Contract>RestResolvingClient`;
/// there is no gRPC resolving client), so every tuning knob applies and there is
/// no gRPC disambiguation or `rest_only_knobs_set` warning to emit here.
///
/// Must be an object: a bare `<dep>: "http://host"` string (the old static-endpoint
/// shape) is rejected at parse time — use the `endpoint` field instead.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConsumerWiring {
    /// Static-endpoint override (ADR-0004 dev/test escape hatch): when set, the
    /// dep is wired to this fixed endpoint instead of being discovered.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// REST client tuning, flattened into the same map as `endpoint`.
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

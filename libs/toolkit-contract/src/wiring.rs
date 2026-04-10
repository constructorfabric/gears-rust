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

    /// Override for the SSE-stream reconnect policy.
    #[serde(default)]
    pub sse_reconnect: Option<ReconnectSettings>,
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

#[cfg(feature = "runtime-client")]
impl ClientTuning {
    /// Apply tuning overrides onto a fresh [`ClientConfig`] built from `endpoint`.
    #[must_use]
    pub fn apply_to(
        &self,
        endpoint: impl Into<String>,
    ) -> crate::runtime::config::ClientConfig {
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
        if let Some(ref s) = self.sse_reconnect {
            let base = cfg.sse_reconnect.clone();
            cfg = cfg.with_sse_reconnect(ReconnectConfig {
                max_attempts: s.max_attempts.unwrap_or(base.max_attempts),
                base_delay: s.base_delay.unwrap_or(base.base_delay),
                max_delay: s.max_delay.unwrap_or(base.max_delay),
            });
        }
        cfg
    }
}

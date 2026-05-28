//! Chat Engine module configuration.
//!
//! Phase 15 owns the load + validate path; all knobs that downstream
//! services need at construction time live on [`ChatEngineConfig`].
//!
//! Defaults are calibrated so the binary boots without a config section —
//! every field is `#[serde(default = …)]` backed by a free helper in
//! [`defaults`].
//
// @cpt-cf-chat-engine-module-config:p15

use serde::Deserialize;
use thiserror::Error;

use crate::domain::service::{
    DEFAULT_PLUGIN_DEADLINE, DEFAULT_STREAMING_BUFFER_SIZE, DEFAULT_SUMMARY_BUFFER_SIZE,
};

/// Validated configuration for the Chat Engine module.
///
/// Loaded via [`modkit::context::ModuleCtx::config_or_default`] and then
/// passed through [`ChatEngineConfig::validate`] before being stored in
/// [`crate::module::ChatEngineModule`]. Every field has a documented
/// default so the module boots even when the `modules.chat-engine.config`
/// section is missing entirely.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatEngineConfig {
    /// Per-call plugin deadline (seconds) applied to streaming hooks
    /// (`on_message`, `on_message_recreate`, `on_session_summary`).
    /// Defaults to [`DEFAULT_PLUGIN_DEADLINE`].
    #[serde(default = "defaults::plugin_deadline_secs")]
    pub plugin_deadline_secs: u64,

    /// Bounded-channel size for the plugin-→-NDJSON sink (ADR-0010).
    /// Defaults to [`DEFAULT_STREAMING_BUFFER_SIZE`]. MUST be > 0.
    #[serde(default = "defaults::ndjson_buffer_size")]
    pub ndjson_buffer_size: usize,

    /// Bounded-channel size for the summary stream. Defaults to
    /// [`DEFAULT_SUMMARY_BUFFER_SIZE`]. MUST be > 0.
    #[serde(default = "defaults::summary_buffer_size")]
    pub summary_buffer_size: usize,

    /// Retention cleanup task tick interval (hours). MUST be > 0.
    #[serde(default = "defaults::retention_cleanup_interval_hours")]
    pub retention_cleanup_interval_hours: u64,

    /// Maximum number of active sessions processed in a single tenant
    /// during one retention tick. Bounds the wall-clock latency of a
    /// tick so a large tenant cannot delay the scheduler indefinitely.
    /// MUST be > 0.
    #[serde(default = "defaults::retention_max_sessions_per_tick")]
    pub retention_max_sessions_per_tick: u32,

    /// Maximum number of eligible subtree roots a single session may
    /// delete during one tick. Bounds both per-session memory use (the
    /// SQL query returns only this many ids) and per-session work
    /// (each id triggers one SERIALIZABLE subtree delete transaction).
    /// MUST be > 0.
    #[serde(default = "defaults::retention_max_deletes_per_session")]
    pub retention_max_deletes_per_session: u32,

    /// Default share token TTL (seconds). When `None`, share tokens
    /// inherit the per-request `expires_in_hours` payload.
    #[serde(default)]
    pub default_share_token_ttl: Option<u64>,

    /// Optional list of webhook endpoints used by the default emitter.
    /// When empty, the module installs a no-op emitter (events still
    /// log at `debug!`).
    #[serde(default)]
    pub webhook_endpoints: Vec<String>,

    /// Base URL for the in-process LLM Gateway. Phase 15 keeps this
    /// optional; concrete `LlmGatewayClient` / `ModelRegistryClient`
    /// impls land alongside this field in the production wiring.
    #[serde(default)]
    pub llm_gateway_base_url: Option<String>,

    /// Public base URL used to compose share links
    /// (`{base}/share/{token}`). When `None`, the module falls back to
    /// the test default (`http://localhost`).
    #[serde(default)]
    pub share_base_url: Option<String>,

    /// Mount the message-search REST endpoints
    /// (`POST /chat-engine/v1/sessions/{id}/search`,
    /// `POST /chat-engine/v1/sessions/search`). Defaults to `false`
    /// because the production `tsvector` (Postgres) and `LIKE`
    /// (SQLite) backends are still stubs that surface
    /// `Internal` for every call — leaving the routes registered
    /// would advertise an endpoint that 500s on every real request.
    /// Operators flip this to `true` once a real backend is wired in
    /// at module construction time.
    #[serde(default)]
    pub enable_search: bool,
}

impl Default for ChatEngineConfig {
    fn default() -> Self {
        Self {
            plugin_deadline_secs: defaults::plugin_deadline_secs(),
            ndjson_buffer_size: defaults::ndjson_buffer_size(),
            summary_buffer_size: defaults::summary_buffer_size(),
            retention_cleanup_interval_hours: defaults::retention_cleanup_interval_hours(),
            retention_max_sessions_per_tick: defaults::retention_max_sessions_per_tick(),
            retention_max_deletes_per_session: defaults::retention_max_deletes_per_session(),
            default_share_token_ttl: None,
            webhook_endpoints: Vec::new(),
            llm_gateway_base_url: None,
            share_base_url: None,
            enable_search: false,
        }
    }
}

impl ChatEngineConfig {
    /// Validate the configuration. Returns a typed error so callers can
    /// short-circuit `init()` with a structured failure rather than a
    /// stringly-typed `anyhow::Error`.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::ZeroBufferSize`] if `ndjson_buffer_size == 0` or
    ///   `summary_buffer_size == 0`.
    /// - [`ConfigError::ZeroRetentionInterval`] if
    ///   `retention_cleanup_interval_hours == 0`.
    /// - [`ConfigError::ZeroRetentionCap`] if either
    ///   `retention_max_sessions_per_tick == 0` or
    ///   `retention_max_deletes_per_session == 0` (a zero cap would
    ///   make every tick a no-op).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.ndjson_buffer_size == 0 {
            return Err(ConfigError::ZeroBufferSize {
                field: "ndjson_buffer_size",
            });
        }
        if self.summary_buffer_size == 0 {
            return Err(ConfigError::ZeroBufferSize {
                field: "summary_buffer_size",
            });
        }
        if self.retention_cleanup_interval_hours == 0 {
            return Err(ConfigError::ZeroRetentionInterval);
        }
        if self.retention_max_sessions_per_tick == 0 {
            return Err(ConfigError::ZeroRetentionCap {
                field: "retention_max_sessions_per_tick",
            });
        }
        if self.retention_max_deletes_per_session == 0 {
            return Err(ConfigError::ZeroRetentionCap {
                field: "retention_max_deletes_per_session",
            });
        }
        Ok(())
    }
}

/// Typed configuration errors surfaced from [`ChatEngineConfig::validate`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// One of the bounded-channel sizes was configured to zero, which
    /// would make `tokio::sync::mpsc::channel` reject the construction.
    #[error("chat-engine config: {field} must be > 0")]
    ZeroBufferSize { field: &'static str },

    /// `retention_cleanup_interval_hours` was zero, which would create a
    /// tight-loop `tokio::time::interval`.
    #[error("chat-engine config: retention_cleanup_interval_hours must be > 0")]
    ZeroRetentionInterval,

    /// One of the retention-cap fields was set to zero — that would
    /// make the retention scheduler a permanent no-op.
    #[error("chat-engine config: {field} must be > 0")]
    ZeroRetentionCap { field: &'static str },
}

mod defaults {
    use super::{DEFAULT_PLUGIN_DEADLINE, DEFAULT_STREAMING_BUFFER_SIZE, DEFAULT_SUMMARY_BUFFER_SIZE};

    pub(super) fn plugin_deadline_secs() -> u64 {
        DEFAULT_PLUGIN_DEADLINE.as_secs()
    }

    pub(super) fn ndjson_buffer_size() -> usize {
        DEFAULT_STREAMING_BUFFER_SIZE
    }

    pub(super) fn summary_buffer_size() -> usize {
        DEFAULT_SUMMARY_BUFFER_SIZE
    }

    pub(super) fn retention_cleanup_interval_hours() -> u64 {
        24
    }

    pub(super) fn retention_max_sessions_per_tick() -> u32 {
        // Tunable per deployment. The default bounds a tick to ~1000
        // SERIALIZABLE subtree deletes in the worst case (combined
        // with retention_max_deletes_per_session = 1).
        1000
    }

    pub(super) fn retention_max_deletes_per_session() -> u32 {
        // Per-session ceiling on subtree-root deletions in a single
        // tick. Older subtree roots that exceed the cap are deferred
        // to the next tick, which keeps a single heavy session from
        // monopolising a tick.
        1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        let cfg = ChatEngineConfig::default();
        cfg.validate().expect("defaults must validate");
        assert!(cfg.plugin_deadline_secs > 0);
        assert!(cfg.ndjson_buffer_size > 0);
        assert!(cfg.summary_buffer_size > 0);
        assert!(cfg.retention_cleanup_interval_hours > 0);
    }

    #[test]
    fn zero_buffer_rejected() {
        let mut cfg = ChatEngineConfig::default();
        cfg.ndjson_buffer_size = 0;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::ZeroBufferSize {
                field: "ndjson_buffer_size"
            }
        ));
    }

    #[test]
    fn zero_summary_buffer_rejected() {
        let mut cfg = ChatEngineConfig::default();
        cfg.summary_buffer_size = 0;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::ZeroBufferSize {
                field: "summary_buffer_size"
            }
        ));
    }

    #[test]
    fn zero_retention_interval_rejected() {
        let mut cfg = ChatEngineConfig::default();
        cfg.retention_cleanup_interval_hours = 0;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::ZeroRetentionInterval));
    }

    #[test]
    fn deserialise_empty_table_uses_defaults() {
        let cfg: ChatEngineConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        cfg.validate().expect("empty config must use defaults");
    }
}

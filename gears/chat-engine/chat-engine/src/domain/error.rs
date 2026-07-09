//! Service-layer error type for the `chat_engine` crate.
//!
//! `ChatEngineError` is the canonical `Result` error used by every service
//! and repository in this crate. It is intentionally framework-agnostic:
//! the conversion into an RFC-9457 problem document happens at the API
//! boundary (Phase 14), not here.
//!
//! Conversions from plugin and generic errors live here:
//! - `chat_engine_sdk::error::PluginError` — failures bubbled up from
//!   backend plugins (routed using the SDK's status / user-facing matrix).
//! - `anyhow::Error` — anything else (always classified as `Internal`).
//!
//! Conversions from database error types (`sea_orm::DbErr`,
//! `toolkit_db::DbError`, `toolkit_db::secure::ScopeError`) live in
//! `infra::db::error_map` so the domain layer stays free of infrastructure
//! imports (DE0301); the `From` impls are in-crate, so `?` still converts.
//
// @cpt-cf-chat-engine-domain-error:p2

use chat_engine_sdk::error::{BoxError, PluginError};
use thiserror::Error;
use toolkit_macros::domain_model;

/// Service-layer error. Each variant carries enough context to be projected
/// into an RFC-9457 `Problem` document by the API layer.
#[domain_model]
#[derive(Debug, Error)]
pub enum ChatEngineError {
    /// A requested resource does not exist. Maps to HTTP 404.
    #[error("{resource} not found: {id}")]
    NotFound {
        /// Logical resource name (e.g., `"session"`, `"message"`).
        resource: &'static str,
        /// Identifier the caller asked for (UUID, name, etc.).
        id: String,
    },

    /// The caller is authenticated but not allowed to perform the action
    /// (tenant/ownership mismatch, missing scope). Maps to HTTP 403.
    #[error("forbidden: {reason}")]
    Forbidden {
        /// Human-readable reason (safe to expose to the client).
        reason: String,
    },

    /// Operation rejected because of a state-machine or concurrency
    /// conflict (invalid lifecycle transition, variant_index race, etc.).
    /// Maps to HTTP 409.
    #[error("conflict: {reason}")]
    Conflict {
        /// Human-readable reason (safe to expose to the client).
        reason: String,
    },

    /// Caller-supplied input was malformed or failed validation. Maps to
    /// HTTP 400.
    #[error("bad request: {reason}")]
    BadRequest {
        /// Human-readable reason (safe to expose to the client).
        reason: String,
    },

    /// A downstream backend (plugin, external service) is unavailable,
    /// rate-limited, or timed out. Maps to HTTP 503 / 504 / 429 — the API
    /// layer decides the exact code from the `retry_after` hint and the
    /// originating `PluginError` variant.
    #[error("backend unavailable: {reason}")]
    BackendUnavailable {
        /// Human-readable reason (operator-facing).
        reason: String,
        /// Optional `Retry-After` hint (currently only set for
        /// `PluginError::RateLimited`).
        retry_after: Option<std::time::Duration>,
        /// Underlying cause if the constructor preserved one.
        #[source]
        source: Option<BoxError>,
    },

    /// Unexpected internal failure — bugs, misconfiguration, unknown DB
    /// errors. Maps to HTTP 500. Details MUST NOT be surfaced to the end
    /// user verbatim; the API layer is responsible for redacting them.
    #[error("internal error: {reason}")]
    Internal {
        /// Operator-facing reason.
        reason: String,
        /// Underlying cause if the constructor preserved one.
        #[source]
        source: Option<BoxError>,
    },

    /// A feature is exposed on the REST surface but its production backend
    /// is not yet wired (object-storage export, DB-backed search). Maps to
    /// HTTP 501. Used to gate placeholder paths so they refuse honestly
    /// instead of faking success (RUST-NO-001).
    #[error("not implemented: {reason}")]
    NotImplemented {
        /// Human-readable reason (safe to expose to the client).
        reason: String,
    },
}

impl ChatEngineError {
    /// Construct a `Conflict` for an invalid lifecycle transition. Used by
    /// `domain::session::ensure_can_transition` and any service that calls
    /// `LifecycleState::can_transition_to` directly.
    #[must_use]
    pub fn invalid_transition(
        from: chat_engine_sdk::models::LifecycleState,
        to: chat_engine_sdk::models::LifecycleState,
    ) -> Self {
        Self::Conflict {
            reason: format!("invalid lifecycle transition: {from} -> {to}"),
        }
    }

    /// Convenience constructor for `NotFound`. Accepts any `Display` id so
    /// callers can pass `Uuid`, `&str`, `String`, …
    pub fn not_found(resource: &'static str, id: impl std::fmt::Display) -> Self {
        Self::NotFound {
            resource,
            id: id.to_string(),
        }
    }

    /// Convenience constructor for `BadRequest`.
    pub fn bad_request(reason: impl Into<String>) -> Self {
        Self::BadRequest {
            reason: reason.into(),
        }
    }

    /// Convenience constructor for `Forbidden`.
    pub fn forbidden(reason: impl Into<String>) -> Self {
        Self::Forbidden {
            reason: reason.into(),
        }
    }

    /// Convenience constructor for `Conflict`.
    pub fn conflict(reason: impl Into<String>) -> Self {
        Self::Conflict {
            reason: reason.into(),
        }
    }

    /// Convenience constructor for `Internal` that does not preserve a cause.
    pub fn internal(reason: impl Into<String>) -> Self {
        Self::Internal {
            reason: reason.into(),
            source: None,
        }
    }

    /// Convenience constructor for `NotImplemented` (HTTP 501).
    pub fn not_implemented(reason: impl Into<String>) -> Self {
        Self::NotImplemented {
            reason: reason.into(),
        }
    }
}

impl From<PluginError> for ChatEngineError {
    fn from(err: PluginError) -> Self {
        match err {
            PluginError::InvalidInput { message, source } => Self::BadRequest {
                reason: source
                    .as_ref()
                    .map_or_else(|| message.clone(), |s| format!("{message}: {s}")),
            },
            PluginError::Unauthorized { message, source } => Self::Forbidden {
                reason: source
                    .as_ref()
                    .map_or_else(|| message.clone(), |s| format!("{message}: {s}")),
            },
            PluginError::NotFound { message, .. } => Self::NotFound {
                resource: "plugin_resource",
                id: message,
            },
            PluginError::RateLimited {
                retry_after,
                source,
            } => Self::BackendUnavailable {
                reason: "backend rate limited".to_string(),
                retry_after,
                source,
            },
            PluginError::Transient { message, source } => Self::BackendUnavailable {
                reason: message,
                retry_after: None,
                source,
            },
            PluginError::Timeout { source } => Self::BackendUnavailable {
                reason: "backend timeout".to_string(),
                retry_after: None,
                source,
            },
            PluginError::Internal { message, source } => Self::Internal {
                reason: message,
                source,
            },
        }
    }
}

impl From<anyhow::Error> for ChatEngineError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal {
            reason: err.to_string(),
            source: Some(err.into()),
        }
    }
}

/// Crate-wide result alias bound to [`ChatEngineError`].
pub type Result<T> = std::result::Result<T, ChatEngineError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;

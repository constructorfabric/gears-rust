use std::time::Duration;

use thiserror::Error;

/// Boxed dynamic error for preserving the underlying cause chain across the
/// SDK boundary. Implements `std::error::Error` so callers can walk the chain
/// via `Error::source()`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors returned by `ChatEngineBackendPlugin` methods.
///
/// Each variant has a clear contract for how Chat Engine reacts when it sees
/// it (HTTP status to surface, retry policy, whether details may be shown to
/// the end user). Use the constructors (`PluginError::transient(...)`,
/// `PluginError::invalid_input_with(..., source)` …) to build values; they
/// preserve the original error chain via the `#[source]` field.
///
/// Routing matrix:
///
/// | Variant         | HTTP | Retry?  | User-facing? | Typical cause                            |
/// |-----------------|------|---------|--------------|------------------------------------------|
/// | `Transient`     | 503  | yes     | no           | network blip, upstream 5xx               |
/// | `RateLimited`   | 429  | yes     | yes          | upstream `Retry-After` / 429             |
/// | `Timeout`       | 504  | yes     | no           | request exceeded the deadline            |
/// | `InvalidInput`  | 400  | no      | yes          | bad request payload, validation failure  |
/// | `Unauthorized`  | 401  | no      | yes          | auth token missing/expired/insufficient  |
/// | `NotFound`      | 404  | no      | yes          | model / resource does not exist          |
/// | `Internal`      | 500  | no      | no (page on-call) | misconfiguration, plugin bug         |
#[derive(Debug, Error)]
pub enum PluginError {
    /// Retryable transient failure (network blip, upstream 5xx, connection reset).
    #[error("transient error: {message}")]
    Transient {
        message: String,
        #[source]
        source: Option<BoxError>,
    },

    /// Upstream rate-limited the request. Retry after the suggested duration if any.
    #[error("rate limited")]
    RateLimited {
        retry_after: Option<Duration>,
        #[source]
        source: Option<BoxError>,
    },

    /// Request exceeded the deadline (or upstream timed out).
    #[error("timeout")]
    Timeout {
        #[source]
        source: Option<BoxError>,
    },

    /// Client-side error: malformed input, validation failure. Surface to user.
    #[error("invalid input: {message}")]
    InvalidInput {
        message: String,
        #[source]
        source: Option<BoxError>,
    },

    /// Authentication or authorization failure. Surface to user.
    #[error("unauthorized: {message}")]
    Unauthorized {
        message: String,
        #[source]
        source: Option<BoxError>,
    },

    /// Resource not found (model, file, session, …). Surface to user.
    #[error("not found: {message}")]
    NotFound {
        message: String,
        #[source]
        source: Option<BoxError>,
    },

    /// Operator-side error: misconfiguration, plugin bug, internal invariant
    /// violation. Hide details from end users; page on-call.
    #[error("internal error: {message}")]
    Internal {
        message: String,
        #[source]
        source: Option<BoxError>,
    },
}

impl PluginError {
    // -------------- Convenience constructors --------------

    pub fn transient(message: impl Into<String>) -> Self {
        Self::Transient {
            message: message.into(),
            source: None,
        }
    }

    pub fn transient_with<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Transient {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self::RateLimited {
            retry_after,
            source: None,
        }
    }

    pub fn rate_limited_with<E>(retry_after: Option<Duration>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::RateLimited {
            retry_after,
            source: Some(Box::new(source)),
        }
    }

    pub fn timeout() -> Self {
        Self::Timeout { source: None }
    }

    pub fn timeout_with<E>(source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Timeout {
            source: Some(Box::new(source)),
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
            source: None,
        }
    }

    pub fn invalid_input_with<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::InvalidInput {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized {
            message: message.into(),
            source: None,
        }
    }

    pub fn unauthorized_with<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Unauthorized {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
            source: None,
        }
    }

    pub fn not_found_with<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::NotFound {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            source: None,
        }
    }

    pub fn internal_with<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Internal {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    // -------------- Routing helpers --------------

    /// Suggested HTTP status code when this error surfaces to a client.
    #[must_use]
    pub fn suggested_status(&self) -> u16 {
        match self {
            Self::InvalidInput { .. } => 400,
            Self::Unauthorized { .. } => 401,
            Self::NotFound { .. } => 404,
            Self::RateLimited { .. } => 429,
            Self::Internal { .. } => 500,
            Self::Timeout { .. } => 504,
            Self::Transient { .. } => 503,
        }
    }

    /// Whether Chat Engine should retry the operation (with backoff).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transient { .. } | Self::RateLimited { .. } | Self::Timeout { .. }
        )
    }

    /// True if the error's message is safe to surface to the end user.
    /// User-actionable errors (`InvalidInput`, `Unauthorized`, `NotFound`,
    /// `RateLimited`) describe user mistakes; the rest may leak operator
    /// details and must be replaced by a generic message at the boundary.
    #[must_use]
    pub fn is_user_facing(&self) -> bool {
        matches!(
            self,
            Self::InvalidInput { .. }
                | Self::Unauthorized { .. }
                | Self::NotFound { .. }
                | Self::RateLimited { .. }
        )
    }

    /// `Retry-After` hint if the variant carries one (currently only
    /// `RateLimited`). Returns `None` for variants without an explicit hint.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_status_codes() {
        assert_eq!(PluginError::transient("x").suggested_status(), 503);
        assert_eq!(PluginError::rate_limited(None).suggested_status(), 429);
        assert_eq!(PluginError::timeout().suggested_status(), 504);
        assert_eq!(PluginError::invalid_input("x").suggested_status(), 400);
        assert_eq!(PluginError::unauthorized("x").suggested_status(), 401);
        assert_eq!(PluginError::not_found("x").suggested_status(), 404);
        assert_eq!(PluginError::internal("x").suggested_status(), 500);
    }

    #[test]
    fn retryable_partition() {
        assert!(PluginError::transient("x").is_retryable());
        assert!(PluginError::rate_limited(None).is_retryable());
        assert!(PluginError::timeout().is_retryable());
        assert!(!PluginError::invalid_input("x").is_retryable());
        assert!(!PluginError::unauthorized("x").is_retryable());
        assert!(!PluginError::not_found("x").is_retryable());
        assert!(!PluginError::internal("x").is_retryable());
    }

    #[test]
    fn user_facing_partition() {
        assert!(PluginError::invalid_input("x").is_user_facing());
        assert!(PluginError::unauthorized("x").is_user_facing());
        assert!(PluginError::not_found("x").is_user_facing());
        assert!(PluginError::rate_limited(None).is_user_facing());
        assert!(!PluginError::transient("x").is_user_facing());
        assert!(!PluginError::timeout().is_user_facing());
        assert!(!PluginError::internal("x").is_user_facing());
    }

    #[test]
    fn source_chain_is_preserved() {
        use std::error::Error;
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "peer reset");
        let plugin_err = PluginError::transient_with("HTTP request failed", io_err);

        // Top-level message
        assert!(plugin_err.to_string().contains("HTTP request failed"));

        // Walk the source chain
        let cause = plugin_err.source().expect("source must be preserved");
        assert!(cause.to_string().contains("peer reset"));
    }

    #[test]
    fn retry_after_only_set_for_rate_limited() {
        assert_eq!(
            PluginError::rate_limited(Some(Duration::from_secs(5))).retry_after(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(PluginError::rate_limited(None).retry_after(), None);
        assert_eq!(PluginError::transient("x").retry_after(), None);
        assert_eq!(PluginError::timeout().retry_after(), None);
    }
}

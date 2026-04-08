use std::collections::HashMap;

use async_trait::async_trait;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;

// ---------------------------------------------------------------------------
// Plugin errors
// ---------------------------------------------------------------------------

/// Errors returned by auth plugins.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("secret not found: {0}")]
    SecretNotFound(String),
    #[error("authentication failed: {0}")]
    #[allow(dead_code)] // Part of plugin trait API; no current plugin constructs this.
    AuthFailed(String),
    #[error("request rejected: {0}")]
    #[allow(dead_code)] // Part of plugin trait API; no current plugin constructs this.
    Rejected(String),
    #[error("invalid plugin configuration: {0}")]
    InvalidConfig(String),
    #[error("plugin error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// Auth plugin
// ---------------------------------------------------------------------------

/// Request context passed to an auth plugin for header injection.
#[domain_model]
pub struct AuthContext {
    /// Outbound request headers (modified in-place by the plugin).
    pub headers: HashMap<String, String>,
    /// Plugin-specific configuration key/value pairs.
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// Trait for outbound authentication plugins.
///
/// Implementations mutate [`AuthContext`] to inject authentication material
/// (e.g., API keys, bearer tokens) into the outbound request headers.
#[async_trait]
pub trait AuthPlugin: Send + Sync {
    /// Apply authentication to the outbound request context.
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError>;
}

// ---------------------------------------------------------------------------
// Guard plugin
// ---------------------------------------------------------------------------

/// Outcome of a guard plugin's request evaluation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Allow the request to proceed to the next pipeline stage.
    Allow,
    /// Reject the request with a specific HTTP status and error code.
    Reject {
        /// HTTP status code for the rejection response (e.g. 400, 403, 429).
        status: u16,
        /// Machine-readable error code for programmatic consumer handling.
        error_code: String,
        /// Human-readable explanation of the rejection.
        detail: String,
    },
}

/// Context passed to a guard plugin for validation.
///
/// Used for both request and response phases. Guard context is immutable —
/// guards validate but do not mutate. Mutation is the responsibility of
/// transform plugins.
///
/// During the request phase, `status` is `None` and `headers` contains the
/// inbound request headers. During the response phase, `status` is
/// `Some(http_status)` and `headers` contains the upstream response headers.
#[domain_model]
#[allow(dead_code)] // Part of guard plugin API; fields read by plugin implementations.
pub struct GuardContext {
    /// HTTP method of the request.
    pub method: String,
    /// Path of the request (after alias resolution).
    pub path: String,
    /// HTTP status code from the upstream response. `None` during request phase.
    pub status: Option<u16>,
    /// Request headers (request phase) or response headers (response phase).
    pub headers: Vec<(String, String)>,
    /// Plugin-specific configuration key/value pairs
    /// (from the plugin binding on the upstream or route).
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// Trait for guard plugins that validate requests and responses.
///
/// Implementations inspect [`GuardContext`] and return a [`GuardDecision`].
/// [`PluginError`] is reserved for plugin infrastructure failures (invalid
/// config, internal errors). Policy decisions use [`GuardDecision::Reject`].
///
/// Both methods default to [`GuardDecision::Allow`], so implementations only
/// need to override the phases they care about.
#[async_trait]
pub trait GuardPlugin: Send + Sync {
    /// Evaluate the inbound request before forwarding to upstream.
    async fn guard_request(&self, _ctx: &GuardContext) -> Result<GuardDecision, PluginError> {
        Ok(GuardDecision::Allow)
    }

    /// Evaluate the upstream response before returning to the client.
    async fn guard_response(&self, _ctx: &GuardContext) -> Result<GuardDecision, PluginError> {
        Ok(GuardDecision::Allow)
    }
}

// ---------------------------------------------------------------------------
// Transform plugin
// ---------------------------------------------------------------------------

/// Mutable request context passed to a transform plugin during the `on_request` phase.
#[domain_model]
#[allow(dead_code)] // Part of transform plugin API; fields read by custom plugin implementations.
pub struct TransformRequestContext {
    /// HTTP method of the request.
    pub method: String,
    /// Path of the request (after alias resolution). Mutable — plugins can rewrite paths.
    pub path: String,
    /// Query parameters. Mutable — plugins can add/remove/modify query params.
    pub query: Vec<(String, String)>,
    /// Request headers. Mutable — plugins can set/add/remove headers.
    pub headers: Vec<(String, String)>,
    /// Plugin-specific configuration key/value pairs
    /// (from the plugin binding on the upstream or route).
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// Mutable response context passed to a transform plugin during the `on_response` phase.
#[domain_model]
#[allow(dead_code)] // Part of transform plugin API; fields read by custom plugin implementations.
pub struct TransformResponseContext {
    /// HTTP status code from the upstream response.
    pub status: u16,
    /// Response headers. Mutable — plugins can set/add/remove headers.
    pub headers: Vec<(String, String)>,
    /// Plugin-specific configuration key/value pairs.
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// Mutable error context passed to a transform plugin during the `on_error` phase.
#[domain_model]
#[allow(dead_code)] // Part of transform plugin API; fields read by custom plugin implementations.
pub struct TransformErrorContext {
    /// The error that occurred during the upstream call.
    pub error_type: String,
    /// HTTP status code that will be returned to the client.
    pub status: u16,
    /// Human-readable error detail. Mutable — plugins can enrich error messages.
    pub detail: String,
    /// Plugin-specific configuration key/value pairs.
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// Trait for transform plugins that mutate request/response/error data.
///
/// Implementations modify context in-place. [`PluginError`] is reserved for
/// plugin infrastructure failures (invalid config, internal errors).
///
/// All methods default to no-op, so implementations only need to override the
/// phases they participate in.
#[async_trait]
pub trait TransformPlugin: Send + Sync {
    /// Mutate the outbound request before forwarding to upstream.
    async fn on_request(&self, _ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
        Ok(())
    }

    /// Mutate the upstream response before returning to the client.
    async fn on_response(&self, _ctx: &mut TransformResponseContext) -> Result<(), PluginError> {
        Ok(())
    }

    /// Mutate the error response before returning to the client.
    async fn on_error(&self, _ctx: &mut TransformErrorContext) -> Result<(), PluginError> {
        Ok(())
    }
}

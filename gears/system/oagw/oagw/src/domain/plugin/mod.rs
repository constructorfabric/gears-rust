pub mod aws_sigv4;
pub mod credentials;

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

// ---------------------------------------------------------------------------
// Plugin errors
// ---------------------------------------------------------------------------

/// Errors returned by auth plugins.
///
/// The proxy maps every auth plugin's errors the same way: a missing secret
/// is 401 `AUTH_CREDENTIAL_NOT_FOUND`, an unavailable credential store 503
/// `CREDENTIAL_STORE_UNAVAILABLE`, an unavailable identity provider 503
/// `IDENTITY_PROVIDER_UNAVAILABLE`, an invalid identity provider response
/// 502, rejected credentials 401 `AUTH_PLUGIN_FAILED`, a
/// misconfiguration 500 and any other plugin failure 401
/// `AUTH_PLUGIN_INTERNAL`.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The secret referenced by the named auth config key does not exist in
    /// the credential store, is not accessible to the tenant, or the
    /// reference is invalid. Carries the key and the credential store's
    /// error kind, never the reference (F0008).
    #[error("secret referenced by auth.config.{key} not accessible: {kind}")]
    SecretNotFound {
        /// The auth config key holding the reference.
        key: &'static str,
        /// The credential store's error kind
        /// (`credentials::credstore_error_kind`).
        kind: &'static str,
    },
    /// The credential store failed or timed out. The detail names the
    /// config key, never a reference or secret.
    #[error("{detail}")]
    CredentialStoreUnavailable {
        /// What was unavailable.
        detail: String,
        /// The store's own retry hint, when it gave one.
        retry_after: Option<std::time::Duration>,
    },
    /// The identity provider's token endpoint failed: transport error,
    /// timeout, 408, 429 or 5xx. Never names a secret.
    #[error("identity provider unavailable: {0}")]
    IdpUnavailable(String),
    /// The identity provider answered with a response the plugin cannot use
    /// (unparseable, missing token, unsupported token type, unexpected
    /// status).
    #[error("invalid identity provider response: {0}")]
    IdpInvalidResponse(String),
    /// The plugin could not authenticate the outbound request: the
    /// credentials were rejected.
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// A guard plugin rejected the request.
    #[error("request rejected: {0}")]
    #[allow(dead_code)] // Part of plugin trait API; no current plugin constructs this.
    Rejected(String),
    /// The plugin's configuration is missing or malformed, or the plugin
    /// returned an output the gateway cannot send.
    #[error("invalid plugin configuration: {0}")]
    InvalidConfig(String),
    /// The plugin cannot handle this request as the caller sent it, though
    /// its configuration is valid: for example a signer that cannot hash a
    /// streamed body. The detail never carries the value the caller sent.
    #[error("{detail}")]
    InvalidRequest {
        /// What the caller sent: a header name or `body`.
        field: &'static str,
        /// Why it cannot be handled, and the fix.
        detail: String,
    },
    /// Unexpected internal failure inside the plugin.
    #[error("plugin error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// Auth plugin
// ---------------------------------------------------------------------------

/// Request context passed to an auth plugin for header injection.
///
/// The plugin does not see the outbound headers: it only lists the ones to
/// set, and the proxy applies them to the outbound request, so repeated or
/// non-UTF-8 client headers pass through unchanged.
#[domain_model]
pub struct AuthContext {
    /// Headers to set on the outbound request, as `(name, value)`. Each
    /// replaces any header of that name the request already carries; an
    /// invalid name or value fails the request.
    pub set_headers: Vec<(String, String)>,
    /// Plugin-specific configuration key/value pairs.
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

impl AuthContext {
    /// A context with no headers set yet.
    #[must_use]
    pub fn new(config: HashMap<String, String>, security_context: SecurityContext) -> Self {
        Self {
            set_headers: Vec::new(),
            config,
            security_context,
        }
    }

    /// Value of the last header named `name` (case-insensitive) the plugin set.
    #[cfg(test)]
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.set_headers
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Body of the outbound request as [`AuthPlugin::sign`] sees it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningBody {
    /// The whole body. Empty for a request without a body and for a
    /// WebSocket handshake.
    Buffered(bytes::Bytes),
    /// The body is streamed to the upstream; its bytes are not known yet.
    Streamed,
}

/// The outbound request as it will be sent, passed to [`AuthPlugin::sign`]
/// (ADR-0023).
#[domain_model]
// Unit tests read the fields, so the expectation holds outside them only.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "scheme, host and port are part of the auth plugin API; the AWS SigV4 plugin signs the Host header instead"
    )
)]
pub struct SigningContext {
    /// HTTP method.
    pub method: String,
    /// URL scheme: `http`, `https` or `wss`.
    pub scheme: String,
    /// Endpoint host as in the URL: an IPv6 address keeps its brackets.
    pub host: String,
    /// Endpoint port, also when the URL leaves out the scheme's default.
    pub port: u16,
    /// Path as sent, percent-encoding included.
    pub path: String,
    /// The gateway removed an empty, `.` or `..` segment from the client's
    /// path, or removed or added a trailing `/`, so [`Self::path`]
    /// may name another resource than the client asked for at an upstream
    /// that does not normalize paths (an S3 object key).
    pub path_rewritten: bool,
    /// Query as sent, without the `?`; `None` when the URL has none.
    pub query: Option<String>,
    /// Headers as sent: lowercase names, every line of a repeated name in
    /// order. `Host` is included. The engine still sets the framing and
    /// connection-scoped headers and joins `Cookie` lines on HTTP/1.1, so a
    /// signer must not cover those.
    pub headers: Vec<(String, Vec<u8>)>,
    /// The request body.
    pub body: SigningBody,
    /// Plugin-specific configuration key/value pairs.
    pub config: HashMap<String, String>,
    /// Security context of the calling subject.
    pub security_context: SecurityContext,
}

/// What [`AuthPlugin::sign`] changes on the outbound request. The default
/// changes nothing.
///
/// `Debug` prints the header names only: the values and the query carry
/// the signature or a credential.
#[domain_model]
#[derive(Default, Clone, PartialEq, Eq)]
pub struct SigningOutput {
    /// Headers to set, as `(name, value)`. Each replaces the lines of that
    /// name and is marked sensitive. An invalid name or value, a name set
    /// twice, or a header the gateway or the engine sets or strips itself
    /// (`Host`, `Content-Length`, `Expect`, hop-by-hop, forwarding,
    /// `X-OAGW-*`), fails the request.
    pub set_headers: Vec<(String, Vec<u8>)>,
    /// What happens to the query.
    pub query: QueryChange,
}

impl std::fmt::Debug for SigningOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.set_headers.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("SigningOutput")
            .field("set_headers", &names)
            .field("query", &self.query)
            .finish()
    }
}

/// The signing phase's change to the query. `Debug` hides a replacement:
/// it may carry a presigned signature.
#[domain_model]
#[derive(Default, Clone, PartialEq, Eq)]
pub enum QueryChange {
    /// Send the query as it is.
    #[default]
    Keep,
    /// Send the URL without a query.
    Remove,
    /// Send this query, without the `?`, exactly as given (no
    /// re-encoding). A query the URI parser would change or reject fails
    /// the request.
    Replace(String),
}

impl std::fmt::Debug for QueryChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => f.write_str("Keep"),
            Self::Remove => f.write_str("Remove"),
            Self::Replace(_) => f.write_str("Replace(..)"),
        }
    }
}

/// Trait for outbound authentication plugins.
///
/// Implementations add authentication material (e.g., API keys, bearer
/// tokens) to [`AuthContext::set_headers`].
#[async_trait]
pub trait AuthPlugin: Send + Sync {
    /// Apply authentication to the outbound request context.
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError>;

    /// Whether the plugin has a signing phase. Only then does the gateway
    /// build the [`SigningContext`] and call [`Self::sign`]: a plugin that
    /// overrides `sign` must return `true` here, or its requests go out
    /// unsigned.
    fn signs(&self) -> bool {
        false
    }

    /// Whether [`Self::sign`] needs the whole body under the upstream's auth
    /// `config`, for a streamed body of `declared_len` bytes (`None`: no
    /// `Content-Length`, so the body would go chunked). A streamed request
    /// body is then read in full (within the body size limit) and signed as
    /// [`SigningBody::Buffered`]; otherwise it stays streamed and `sign`
    /// sees [`SigningBody::Streamed`]. Asked only when [`Self::signs`] is
    /// true.
    fn signs_body(&self, _config: &HashMap<String, String>, _declared_len: Option<u64>) -> bool {
        true
    }

    /// Sign the final outbound request (ADR-0023). Runs once per request,
    /// when [`Self::signs`] is true, after header rules, transforms,
    /// endpoint selection and the URL build, right before the request is
    /// sent. An error stops the request.
    ///
    /// Runs under the request deadline: the future may be dropped at any
    /// await, so an implementation must not leave shared state (a nonce, a
    /// cached credential) half-updated across one.
    async fn sign(&self, _ctx: &SigningContext) -> Result<SigningOutput, PluginError> {
        Ok(SigningOutput::default())
    }
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
        /// Identifier of the resource the rejection refers to, when the
        /// guard can name one. Lets the REST mapping route 404/409
        /// rejections to the canonical `not_found` / `already_exists`
        /// categories with a real `resource_name` instead of falling
        /// back to `failed_precondition` / `aborted`.
        resource_id: Option<String>,
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
    ///
    /// Holds only values that are visible ASCII; other lines (obs-text,
    /// non-UTF-8) pass through unseen. Lines of a name the plugin leaves as
    /// it found them keep their exact bytes and sensitive flag; changing a
    /// name replaces all its lines, unseen ones included.
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
    /// Same view and write-back as [`TransformRequestContext::headers`].
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

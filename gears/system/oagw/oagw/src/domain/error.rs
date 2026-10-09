use oagw_sdk::{field, reason};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::repo::RepositoryError;

/// Domain-layer errors for OAGW control-plane and data-plane operations.
#[domain_model]
#[derive(Debug, Clone, thiserror::Error)]
pub enum DomainError {
    /// The requested entity does not exist.
    #[error("{entity} not found: {id}")]
    NotFound {
        /// Kind of entity looked up (for example `upstream` or `route`).
        entity: &'static str,
        /// ID that was not found.
        id: Uuid,
    },

    /// A uniqueness constraint was violated.
    #[error("{entity} conflict on {resource}: {detail}")]
    Conflict {
        /// Kind of entity involved.
        entity: &'static str,
        /// Name of the conflicting resource (for example the alias).
        resource: String,
        /// Human-readable description of the conflict.
        detail: String,
    },

    /// The request failed validation.
    #[error("validation [{field}/{reason}]: {detail}")]
    Validation {
        /// Field path the violation is about (e.g. `"content-length"`,
        /// `"gts_id"`). Empty when the caller can't pin a single field —
        /// the REST mapping then emits the canonical `Format` variant
        /// instead of a misleading per-field violation.
        field: &'static str,
        /// Stable, machine-readable code for the violation
        /// (e.g. `"INVALID_GTS_FORMAT"`, `"WS_UPGRADE_REQUIRES_GET"`).
        reason: &'static str,
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The target upstream exists but is disabled.
    #[error("upstream '{alias}' is disabled")]
    UpstreamDisabled {
        /// Alias of the disabled upstream.
        alias: String,
    },

    /// Unexpected internal failure.
    #[error("internal: {message}")]
    Internal {
        /// Description of the failure (not necessarily safe to expose to clients).
        message: String,
    },

    /// A multi-endpoint upstream was called without a target host header.
    #[error("target host header required for multi-endpoint upstream")]
    MissingTargetHost {
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The target host header is malformed.
    #[error("invalid target host header format")]
    InvalidTargetHost {
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The target host header names a host that is not an endpoint of the upstream.
    #[error("{detail}")]
    UnknownTargetHost {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// Authentication towards the upstream failed.
    #[error("[{reason}] {detail}")]
    AuthenticationFailed {
        /// Stable, machine-readable subcategory of the failure
        /// (e.g. `"AUTH_PLUGIN_NOT_FOUND"`, `"AUTH_PLUGIN_FAILED"`,
        /// `"AUTH_PLUGIN_INTERNAL"`). Surfaces on the wire as the
        /// `unauthenticated.reason` field so clients can branch
        /// programmatically without parsing `detail`.
        reason: &'static str,
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The request or response body exceeds the allowed size.
    #[error("{detail}")]
    PayloadTooLarge {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// A rate limit was exceeded.
    #[error("{detail}")]
    RateLimitExceeded {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
        /// Seconds the caller should wait before retrying, when known.
        retry_after_secs: Option<u64>,
        /// Configured bucket capacity, when known.
        limit: Option<u64>,
        /// Tokens remaining in the bucket, when known.
        remaining: Option<u64>,
        /// Unix epoch timestamp at which the bucket will be full again, when known.
        reset_epoch: Option<u64>,
    },

    /// A secret referenced by the upstream auth config could not be found at request time.
    #[error("{detail}")]
    SecretNotFound {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// A management-plane `secret_ref` did not resolve to an accessible
    /// secret — not provisioned yet, or not shared with the tenant. A state
    /// precondition rather than a malformed argument: the same request
    /// succeeds once the secret becomes accessible, so in-process callers
    /// (gear provisioning loops) can distinguish it from `Validation` and
    /// retry. Maps to canonical `failed_precondition` (HTTP 400), unlike the
    /// request-time [`DomainError::SecretNotFound`] which is a server-side
    /// config failure (500).
    #[error("{detail}")]
    SecretRefNotAccessible {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The upstream returned an invalid or failing response.
    #[error("{detail}")]
    DownstreamError {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The upstream violated the protocol (e.g. malformed HTTP).
    #[error("{detail}")]
    ProtocolError {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// Connecting to the upstream timed out.
    #[error("{detail}")]
    ConnectionTimeout {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The upstream did not respond within the request timeout.
    #[error("{detail}")]
    RequestTimeout {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// A guard plugin rejected the request with a specific status and error code.
    #[error("guard rejected: {detail}")]
    GuardRejected {
        /// HTTP status code chosen by the guard plugin.
        status: u16,
        /// Machine-readable error code chosen by the guard plugin.
        error_code: String,
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
        /// Optional identifier of the resource the rejection refers to.
        /// Lets the REST mapping route 404/409 rejections to canonical
        /// `not_found` / `already_exists` instead of the
        /// `failed_precondition` / `aborted` fallback.
        resource_id: Option<String>,
    },

    /// CORS: the request origin is not in the allowed origins list.
    #[error("CORS origin not allowed: {origin}")]
    CorsOriginNotAllowed {
        /// The rejected `Origin` header value.
        origin: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// CORS: the request method is not in the allowed methods list.
    #[error("CORS method not allowed: {method}")]
    CorsMethodNotAllowed {
        /// The rejected request method.
        method: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The upstream response stream was aborted mid-transfer.
    #[error("{detail}")]
    StreamAborted {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The upstream link is unavailable (e.g. no healthy endpoint or connection refused).
    #[error("{detail}")]
    LinkUnavailable {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The circuit breaker for the upstream is open.
    #[error("{detail}")]
    CircuitBreakerOpen {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// The connection was idle for longer than the idle timeout.
    #[error("{detail}")]
    IdleTimeout {
        /// Human-readable explanation of the failure.
        detail: String,
        /// Request instance URI the error relates to (used as the problem `instance`).
        instance: String,
    },

    /// A referenced plugin is not registered.
    #[error("plugin not found: {gts_id}: {detail}")]
    PluginNotFound {
        /// GTS ID of the missing plugin.
        gts_id: String,
        /// Human-readable explanation of the failure.
        detail: String,
    },

    /// The plugin cannot be removed because it is still referenced.
    #[error("plugin in use: {gts_id}: {detail}")]
    PluginInUse {
        /// GTS ID of the plugin in use.
        gts_id: String,
        /// Human-readable explanation of the failure.
        detail: String,
    },

    /// The request was denied by the authorization policy.
    #[error("access forbidden [{reason}]: {detail}")]
    Forbidden {
        /// Stable, machine-readable code identifying the policy
        /// rule or subsystem that denied the request. Comes from
        /// `EnforcerError::Denied.deny_reason.error_code` when the
        /// PEP supplies one, or from a fixed taxonomy
        /// (`AUTHZ_DENIED`, `TENANT_RESOLVER_UNAUTHORIZED`, …)
        /// otherwise. Surfaces on the wire as the
        /// `permission_denied.reason` field — clients branch on
        /// this, not on `detail`.
        reason: String,
        /// Human-readable explanation of the failure.
        detail: String,
    },
}

impl DomainError {
    /// Construct a [`DomainError::NotFound`] for the given entity kind and ID.
    #[must_use]
    pub fn not_found(entity: &'static str, id: Uuid) -> Self {
        Self::NotFound { entity, id }
    }

    /// Construct a [`DomainError::Conflict`] for the given entity kind, resource and detail.
    #[must_use]
    pub fn conflict(
        entity: &'static str,
        resource: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Conflict {
            entity,
            resource: resource.into(),
            detail: detail.into(),
        }
    }

    /// Construct a generic validation error without a specific field.
    /// Sites that know which input was bad should use [`Self::validation_for`]
    /// instead so the wire response can pin the offending field.
    #[must_use]
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            field: "",
            reason: field::VALIDATION,
            detail: detail.into(),
            instance: String::new(),
        }
    }

    /// Construct a validation error scoped to a specific field with a
    /// stable reason code. The field name lands in
    /// `context.field_violations[].field` on the wire, and the reason in
    /// `context.field_violations[].reason`.
    #[must_use]
    pub fn validation_for(
        field: &'static str,
        reason: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self::Validation {
            field,
            reason,
            detail: detail.into(),
            instance: String::new(),
        }
    }

    /// Construct a [`DomainError::UpstreamDisabled`] for the given upstream alias.
    #[must_use]
    pub fn upstream_disabled(alias: impl Into<String>) -> Self {
        Self::UpstreamDisabled {
            alias: alias.into(),
        }
    }

    /// Construct a [`DomainError::Internal`] with the given message.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// Construct a [`DomainError::Forbidden`] with the given detail message
    /// and the default `AUTHZ_DENIED` reason. Sites that have a more
    /// specific stable code should call [`Self::forbidden_with_reason`].
    #[must_use]
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::Forbidden {
            reason: reason::permission::AUTHZ_DENIED.into(),
            detail: detail.into(),
        }
    }

    /// Construct a [`DomainError::Forbidden`] with an explicit machine-readable
    /// reason code. The reason flows to `permission_denied.reason` on the wire.
    #[must_use]
    pub fn forbidden_with_reason(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Forbidden {
            reason: reason.into(),
            detail: detail.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// From<RepositoryError>
// ---------------------------------------------------------------------------

impl From<RepositoryError> for DomainError {
    fn from(e: RepositoryError) -> Self {
        match e {
            RepositoryError::NotFound { entity, id } => Self::NotFound { entity, id },
            RepositoryError::Conflict {
                entity,
                resource,
                detail,
            } => Self::Conflict {
                entity,
                resource,
                detail,
            },
            RepositoryError::Internal(message) => Self::Internal { message },
        }
    }
}

// ---------------------------------------------------------------------------
// From<TenantResolverError>
// ---------------------------------------------------------------------------

impl From<tenant_resolver_sdk::TenantResolverError> for DomainError {
    fn from(e: tenant_resolver_sdk::TenantResolverError) -> Self {
        use tenant_resolver_sdk::TenantResolverError;

        match e {
            TenantResolverError::TenantNotFound { tenant_id } => {
                tracing::warn!(tenant_id = %tenant_id, "tenant not found during hierarchy resolution");
                Self::NotFound {
                    entity: "tenant",
                    id: tenant_id.0,
                }
            }
            TenantResolverError::Unauthorized => Self::Forbidden {
                reason: reason::permission::TENANT_RESOLVER_UNAUTHORIZED.into(),
                detail: "tenant resolver: unauthorized".to_string(),
            },
            TenantResolverError::NoPluginAvailable => Self::Internal {
                message: "tenant resolver: no plugin available".to_string(),
            },
            TenantResolverError::ServiceUnavailable(msg) => Self::Internal {
                message: format!("tenant resolver unavailable: {msg}"),
            },
            TenantResolverError::Internal(msg) => Self::Internal {
                message: format!("tenant resolver internal error: {msg}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// From<EnforcerError>
// ---------------------------------------------------------------------------

/// Convert an authorization enforcer error into a domain error.
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        use authz_resolver_sdk::EnforcerError;

        tracing::error!(error = %e, "OAGW authorization check failed");
        match e {
            EnforcerError::Denied { deny_reason } => match deny_reason {
                Some(r) => Self::Forbidden {
                    reason: r.error_code,
                    detail: r
                        .details
                        .unwrap_or_else(|| "access denied by policy".into()),
                },
                None => Self::Forbidden {
                    reason: reason::permission::AUTHZ_DENIED.into(),
                    detail: "access denied by policy".into(),
                },
            },
            EnforcerError::CompileFailed(_) => Self::Internal {
                message: "authorization constraint compilation failed".to_string(),
            },
            EnforcerError::EvaluationFailed(_) => Self::Internal {
                message: "authorization evaluation failed".to_string(),
            },
        }
    }
}

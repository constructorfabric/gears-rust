use modkit_macros::domain_model;
use uuid::Uuid;

use super::repo::RepositoryError;

/// Domain-layer errors for OAGW control-plane and data-plane operations.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("{entity} not found: {id}")]
    NotFound { entity: &'static str, id: Uuid },

    #[error("{entity} conflict on {resource}: {detail}")]
    Conflict {
        entity: &'static str,
        resource: String,
        detail: String,
    },

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
        detail: String,
        instance: String,
    },

    #[error("upstream '{alias}' is disabled")]
    UpstreamDisabled { alias: String },

    #[error("internal: {message}")]
    Internal { message: String },

    #[error("target host header required for multi-endpoint upstream")]
    MissingTargetHost { instance: String },

    #[error("invalid target host header format")]
    InvalidTargetHost { instance: String },

    #[error("{detail}")]
    UnknownTargetHost { detail: String, instance: String },

    #[error("[{reason}] {detail}")]
    AuthenticationFailed {
        /// Stable, machine-readable subcategory of the failure
        /// (e.g. `"AUTH_PLUGIN_NOT_FOUND"`, `"AUTH_PLUGIN_FAILED"`,
        /// `"AUTH_PLUGIN_INTERNAL"`). Surfaces on the wire as the
        /// `unauthenticated.reason` field so clients can branch
        /// programmatically without parsing `detail`.
        reason: &'static str,
        detail: String,
        instance: String,
    },

    #[error("{detail}")]
    PayloadTooLarge { detail: String, instance: String },

    #[error("{detail}")]
    RateLimitExceeded {
        detail: String,
        instance: String,
        retry_after_secs: Option<u64>,
        limit: Option<u64>,
        remaining: Option<u64>,
        reset_epoch: Option<u64>,
    },

    #[error("{detail}")]
    SecretNotFound { detail: String, instance: String },

    #[error("{detail}")]
    DownstreamError { detail: String, instance: String },

    #[error("{detail}")]
    ProtocolError { detail: String, instance: String },

    #[error("{detail}")]
    ConnectionTimeout { detail: String, instance: String },

    #[error("{detail}")]
    RequestTimeout { detail: String, instance: String },

    /// A guard plugin rejected the request with a specific status and error code.
    #[error("guard rejected: {detail}")]
    GuardRejected {
        status: u16,
        error_code: String,
        detail: String,
        instance: String,
        /// Optional identifier of the resource the rejection refers to.
        /// Lets the REST mapping route 404/409 rejections to canonical
        /// `not_found` / `already_exists` instead of the
        /// `failed_precondition` / `aborted` fallback.
        resource_id: Option<String>,
    },

    /// CORS: the request origin is not in the allowed origins list.
    #[error("CORS origin not allowed: {origin}")]
    CorsOriginNotAllowed { origin: String, instance: String },

    /// CORS: the request method is not in the allowed methods list.
    #[error("CORS method not allowed: {method}")]
    CorsMethodNotAllowed { method: String, instance: String },

    #[error("{detail}")]
    StreamAborted { detail: String, instance: String },

    #[error("{detail}")]
    LinkUnavailable { detail: String, instance: String },

    #[error("{detail}")]
    CircuitBreakerOpen { detail: String, instance: String },

    #[error("{detail}")]
    IdleTimeout { detail: String, instance: String },

    #[error("plugin not found: {gts_id}: {detail}")]
    PluginNotFound { gts_id: String, detail: String },

    #[error("plugin in use: {gts_id}: {detail}")]
    PluginInUse { gts_id: String, detail: String },

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
        detail: String,
    },
}

impl DomainError {
    #[must_use]
    pub fn not_found(entity: &'static str, id: Uuid) -> Self {
        Self::NotFound { entity, id }
    }

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
            reason: "VALIDATION",
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

    #[must_use]
    pub fn upstream_disabled(alias: impl Into<String>) -> Self {
        Self::UpstreamDisabled {
            alias: alias.into(),
        }
    }

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
            reason: "AUTHZ_DENIED".into(),
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
                reason: "TENANT_RESOLVER_UNAUTHORIZED".into(),
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
                    reason: "AUTHZ_DENIED".into(),
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

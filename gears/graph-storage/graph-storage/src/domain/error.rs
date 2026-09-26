//! Domain error vocabulary — one authoritative classification per failure
//! (DESIGN § Error Model). The REST adapter and the `ClientHub` adapter both
//! render from this type via one `From` impl, so they can never classify one
//! failure differently.

use graph_storage_sdk::models::ItemError;
use thiserror::Error;
use toolkit_macros::domain_model;

/// Stable machine-readable reasons — a published vocabulary; clients never
/// parse human-readable detail strings.
pub mod reasons {
    pub const SCHEMA_VIOLATION: &str = "SCHEMA_VIOLATION";
    /// A request the gear cannot interpret: an unknown enumeration value, a
    /// query option it does not take, a malformed migration step.
    pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
    /// Two bounds that cannot hold at once (a mode that needs a query text
    /// without one, a traversal without a seed).
    pub const LIMIT_COMBINATION: &str = "LIMIT_COMBINATION";
    pub const LIMIT_EXCEEDED: &str = "LIMIT_EXCEEDED";
    pub const CAS_CONFLICT: &str = "CAS_CONFLICT";
    pub const SERIALIZATION: &str = "SERIALIZATION";
    pub const STALE_GENERATION: &str = "STALE_GENERATION";
    pub const IDEMPOTENCY_MISMATCH: &str = "IDEMPOTENCY_MISMATCH";
    pub const IDEMPOTENCY_KEY_EXPIRED: &str = "IDEMPOTENCY_KEY_EXPIRED";
    pub const DEADLINE: &str = "DEADLINE";
    pub const CANCELLED: &str = "CANCELLED";
    pub const CAPABILITY_UNSUPPORTED: &str = "CAPABILITY_UNSUPPORTED";
    pub const SCOPE_UNSERVABLE: &str = "SCOPE_UNSERVABLE";
    pub const DEPENDENCY_UNAVAILABLE: &str = "DEPENDENCY_UNAVAILABLE";
    pub const EMBEDDING_SPACE_MISMATCH: &str = "EMBEDDING_SPACE_MISMATCH";
    pub const SOURCE_NAMESPACE_FORBIDDEN: &str = "SOURCE_NAMESPACE_FORBIDDEN";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const STORE_CORRUPT: &str = "STORE_CORRUPT";
    pub const INTERNAL: &str = "INTERNAL";
}

#[domain_model]
#[derive(Debug, Error)]
pub enum DomainError {
    /// Malformed request or per-item schema violations. A batch with any item
    /// error committed nothing.
    #[error("{} item(s) failed validation", items.len())]
    Validation { items: Vec<ItemError> },

    /// Malformed request outside the per-item shape.
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    /// Two request bounds that cannot hold at once: a mode that needs a
    /// query text sent without one, a traversal without a seed. Its own
    /// variant so the stable reason says `LIMIT_COMBINATION` only when that
    /// is what happened.
    #[error("inconsistent request: {message}")]
    LimitCombination { message: String },

    /// A malformed query: an unknown filter field, an unparseable cursor, an
    /// ordering the store cannot serve. Its own variant so the stable reason
    /// says `SCHEMA_VIOLATION` — telling a client that named a field which
    /// does not exist that they combined limits wrongly sends them the wrong
    /// way, and `reason` is what a client matches on.
    #[error("invalid query: {message}")]
    InvalidQuery { message: String },

    /// A documented hard bound exceeded (`out_of_range`); never retry
    /// unchanged.
    #[error("limit exceeded: {what}")]
    LimitExceeded { what: String },

    /// Same-key different-type ingest, expected-version mismatch, or an
    /// equal-generation replacement with different content (`aborted`).
    #[error("conflict: {reason}")]
    CasConflict { reason: String },

    /// Serialization failure under concurrent ingest (`aborted`); retry
    /// unchanged.
    #[error("serialization failure under concurrent ingest")]
    Serialization,

    /// Older source generation for a scope (`failed_precondition`); drop the
    /// stale run.
    #[error("stale source generation: recorded {recorded}, offered {offered}")]
    StaleGeneration { recorded: i64, offered: i64 },

    /// Idempotency key reused with a different request (`aborted`).
    #[error("idempotency key reused with a different request")]
    IdempotencyMismatch,

    /// Receipt expired or from a previous epoch (`failed_precondition`).
    #[error("idempotency receipt expired; reconcile and issue a new request")]
    IdempotencyExpired,

    /// Unauthorized or unknown — indistinguishable by contract
    /// (anti-enumeration).
    #[error("not found")]
    NotFound,

    /// Operation-level denial (not resource-level, which answers `NotFound`).
    #[error("access denied")]
    AccessDenied,

    /// A write under a source namespace bound to another producer principal
    /// (`permission_denied`). The one denial that is *not* disguised as
    /// absence: the caller named a namespace whose owner is a fact about the
    /// tenant rather than about them, and "not found" would send them to
    /// create what already exists. Never retry; request a transfer.
    #[error("source namespace `{namespace}` is owned by another producer")]
    SourceNamespaceForbidden { namespace: String },

    /// No implementation can serve the caller's scope shape
    /// (`failed_precondition`); reached only when the fallback chain is
    /// exhausted.
    #[error("no implementation can serve this scope: {reason}")]
    ScopeUnservable { reason: String },

    /// Stored vectors and the active provider belong to different embedding
    /// spaces, so nothing can rank one against the other
    /// (`failed_precondition`). Only the vector arm is affected: every other
    /// path serves the same rows it always did.
    #[error("vector search unavailable: {reason}")]
    VectorSearchUnavailable { reason: String },

    /// Capability not supported by the selected implementation
    /// (`unimplemented`).
    #[error("capability unsupported: {what}")]
    Unsupported { what: String },

    /// Dependency unavailable (`unavailable`); wait and retry.
    #[error("dependency unavailable: {detail}")]
    Unavailable { detail: String },

    #[error("operation exceeded its deadline")]
    Deadline,

    #[error("operation cancelled")]
    Cancelled,

    /// Durable corruption detected (`data_loss`); operator action.
    #[error("store corrupt: {reason}")]
    Corrupt { reason: String },

    /// Unexpected failure (`unknown`); protected diagnostics stay in logs.
    #[error("internal error")]
    Internal { detail: String },
}

impl DomainError {
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            message: message.into(),
        }
    }

    pub fn limit_combination(message: impl Into<String>) -> Self {
        Self::LimitCombination {
            message: message.into(),
        }
    }
}

impl From<graph_storage_sdk::plugin_api::GraphStoreError> for DomainError {
    fn from(error: graph_storage_sdk::plugin_api::GraphStoreError) -> Self {
        use graph_storage_sdk::plugin_api::GraphStoreError as E;
        match error {
            E::ScopeUnservable { reason } => Self::ScopeUnservable { reason },
            E::Unsupported { what } => Self::Unsupported { what: what.into() },
            E::Validation { items } => Self::Validation { items },
            E::Conflict { reason } => Self::CasConflict { reason },
            E::Serialization => Self::Serialization,
            E::StaleGeneration { recorded, offered } => Self::StaleGeneration { recorded, offered },
            E::IdempotencyMismatch => Self::IdempotencyMismatch,
            E::IdempotencyExpired => Self::IdempotencyExpired,
            E::NotFound => Self::NotFound,
            E::LimitExceeded { what } => Self::LimitExceeded { what },
            E::SourceNamespaceForbidden { namespace } => {
                Self::SourceNamespaceForbidden { namespace }
            }
            E::InvalidQuery { what } => Self::InvalidQuery { message: what },
            E::Corrupt { reason } => Self::Corrupt { reason },
            E::Unavailable { reason } => Self::Unavailable { detail: reason },
            E::Deadline => Self::Deadline,
            E::Cancelled => Self::Cancelled,
            E::Internal(detail) => Self::Internal { detail },
            other => Self::Internal {
                detail: format!("unclassified store error: {other}"),
            },
        }
    }
}

impl From<graph_storage_sdk::plugin_api::GraphEngineError> for DomainError {
    fn from(error: graph_storage_sdk::plugin_api::GraphEngineError) -> Self {
        use graph_storage_sdk::plugin_api::GraphEngineError as E;
        match error {
            E::ScopeNotEnforceable { reason } => Self::ScopeUnservable { reason },
            E::Unsupported { what } => Self::Unsupported { what: what.into() },
            E::Unavailable { reason } => Self::Unavailable { detail: reason },
            E::Deadline => Self::Deadline,
            E::Cancelled => Self::Cancelled,
            E::Internal(detail) => Self::Internal { detail },
            other => Self::Internal {
                detail: format!("unclassified engine error: {other}"),
            },
        }
    }
}

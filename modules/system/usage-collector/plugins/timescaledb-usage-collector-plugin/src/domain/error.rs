//! Error types for the `TimescaleDB` storage plugin.
use modkit_macros::domain_model;

use thiserror::Error;
use usage_collector_sdk::{UsageCollectorError, UsageRecordError};

/// Boxed, type-erased source error preserved for `std::error::Error::source()`.
///
/// Domain models cannot reference `sqlx::Error` directly (infrastructure
/// dependency banned by `#[domain_model]`), so infra layers box the
/// underlying database error into this alias before constructing a
/// [`StoragePluginError`]. Walking `source()` on the resulting error still
/// reaches the original `sqlx::Error` for triage, and the infra wrapper's
/// `Display` includes the SQLSTATE.
pub type SourceError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Stable reason codes embedded in the detail of mapped [`UsageCollectorError`]s.
///
/// Operators triaging a production failure can grep for these strings to
/// distinguish a migration failure from a continuous-aggregate setup failure
/// from a query failure without parsing the prose detail.
mod reason {
    pub const MIGRATION: &str = "STORAGE_MIGRATION_FAILED";
    pub const CAGG_SETUP: &str = "STORAGE_CAGG_SETUP_FAILED";
    pub const RETENTION_SETUP: &str = "STORAGE_RETENTION_SETUP_FAILED";
    pub const QUERY: &str = "STORAGE_QUERY_FAILED";
    pub const CONNECTION_POOL: &str = "STORAGE_CONNECTION_POOL";
    pub const UNEXPECTED_UNIQUE: &str = "STORAGE_UNEXPECTED_UNIQUE_VIOLATION";
    pub const CONFIGURATION: &str = "STORAGE_CONFIGURATION";
    pub const TRANSIENT: &str = "STORAGE_TRANSIENT";
}

/// Errors produced by the `TimescaleDB` storage plugin.
///
/// Variants that originate from a `sqlx::Error` carry the original error as a
/// `#[source]`, so `std::error::Error::source()` walks the chain and the
/// SQLSTATE on `sqlx::Error::Database` is reachable for triage. The
/// `From<StoragePluginError> for UsageCollectorError` impl folds the chain
/// (including the SQLSTATE) into the public detail string with a stable
/// reason-code prefix.
#[derive(Debug, Error)]
#[domain_model]
pub enum StoragePluginError {
    /// A record field failed validation (e.g. negative counter value, missing idempotency key).
    #[error("invalid record: {0}")]
    InvalidRecord(String),

    /// A `usage_records` insert collided with an existing row on the
    /// `(tenant_id, idempotency_key)` index *after* the idempotency-key claim
    /// said the slot was free. This is an internal invariant break — callers
    /// upstream of the claim must have re-issued the same key concurrently.
    #[error("unexpected unique constraint violation")]
    UnexpectedUniqueViolation(#[source] SourceError),

    /// A transient database error (connection lost, pool timeout, serialization failure).
    #[error("transient database error")]
    Transient(#[source] SourceError),

    /// A configuration error detected at startup (missing URL, TLS rejected, etc.).
    #[error("configuration error: {0}")]
    Configuration(String),

    /// A schema migration step failed.
    #[error("schema migration failed: {context}")]
    Migration {
        context: String,
        #[source]
        source: SourceError,
    },

    /// The continuous aggregate setup step failed.
    ///
    /// `source` is `None` for post-setup invariant checks that do not bubble
    /// up an underlying database error (e.g. the view exists but the refresh
    /// policy is missing).
    #[error("continuous aggregate setup failed: {context}")]
    ContinuousAggregateSetupFailed {
        context: String,
        #[source]
        source: Option<SourceError>,
    },

    /// The retention policy setup step failed.
    #[error("retention policy setup failed: {context}")]
    RetentionPolicySetupFailed {
        context: String,
        #[source]
        source: SourceError,
    },

    /// A query against the database failed.
    #[error("query failed")]
    QueryFailed(#[source] SourceError),

    /// Row decoding, argument binding, or a structural overflow encountered
    /// while constructing or unpacking a query failed. Distinct from
    /// [`QueryFailed`] because the underlying error is not a `sqlx::Error`
    /// reaching the database — it is a serialization-layer fault on either
    /// side of the wire. Routed through the same `STORAGE_QUERY_FAILED`
    /// reason-code prefix so operators can grep one string for any
    /// query-pipeline issue.
    #[error("query serialization error: {context}")]
    Serialization {
        context: String,
        #[source]
        source: SourceError,
    },

    /// A connection pool error (pool exhausted, pool creation failed, etc.).
    #[error("connection pool error: {0}")]
    ConnectionPool(String),
}

/// Type alias for migration-related errors.
pub type MigrationError = StoragePluginError;

/// Builds an operator-facing detail string that walks the `source()` chain of
/// `err`. The infra-layer database wrapper (`crate::infra::db_error::DbError`)
/// embeds the SQLSTATE in its `Display`, so a generic chain walk surfaces it
/// without needing to downcast to `sqlx::Error` from the domain layer.
///
/// Without this walk, `format!("{err}")` only renders the top-level
/// `Display`, hiding the underlying database error we deliberately preserved
/// on the variant.
fn render_source_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut rendered = err.to_string();
    let mut current = err.source();
    while let Some(src) = current {
        rendered.push_str(": ");
        rendered.push_str(&src.to_string());
        current = src.source();
    }
    rendered
}

impl From<StoragePluginError> for UsageCollectorError {
    fn from(err: StoragePluginError) -> Self {
        match &err {
            StoragePluginError::InvalidRecord(msg) => UsageRecordError::invalid_argument()
                .with_constraint(format!("invalid record: {msg}"))
                .create(),
            StoragePluginError::UnexpectedUniqueViolation(_) => {
                tracing::warn!(
                    error = %err,
                    "unexpected unique constraint violation in usage_records"
                );
                UsageCollectorError::internal(format!(
                    "[{}] {}",
                    reason::UNEXPECTED_UNIQUE,
                    render_source_chain(&err)
                ))
                .create()
            }
            StoragePluginError::Transient(_) => UsageCollectorError::service_unavailable()
                .with_detail(format!(
                    "[{}] {}",
                    reason::TRANSIENT,
                    render_source_chain(&err)
                ))
                .create(),
            StoragePluginError::Configuration(msg) => UsageCollectorError::internal(format!(
                "[{}] configuration error: {msg}",
                reason::CONFIGURATION
            ))
            .create(),
            StoragePluginError::Migration { .. } => UsageCollectorError::internal(format!(
                "[{}] {}",
                reason::MIGRATION,
                render_source_chain(&err)
            ))
            .create(),
            StoragePluginError::ContinuousAggregateSetupFailed { .. } => {
                UsageCollectorError::internal(format!(
                    "[{}] {}",
                    reason::CAGG_SETUP,
                    render_source_chain(&err)
                ))
                .create()
            }
            StoragePluginError::RetentionPolicySetupFailed { .. } => {
                UsageCollectorError::internal(format!(
                    "[{}] {}",
                    reason::RETENTION_SETUP,
                    render_source_chain(&err)
                ))
                .create()
            }
            StoragePluginError::QueryFailed(_) | StoragePluginError::Serialization { .. } => {
                UsageCollectorError::internal(format!(
                    "[{}] {}",
                    reason::QUERY,
                    render_source_chain(&err)
                ))
                .create()
            }
            StoragePluginError::ConnectionPool(msg) => UsageCollectorError::internal(format!(
                "[{}] connection pool error: {msg}",
                reason::CONNECTION_POOL
            ))
            .create(),
        }
    }
}

/// Errors produced by the scope-to-SQL translator.
#[derive(Debug, Error)]
#[domain_model]
pub enum ScopeTranslationError {
    /// The scope has no constraints — callers must fail closed on empty scope.
    #[error("empty scope: access denied")]
    EmptyScope,

    /// A predicate type that cannot be translated to SQL (e.g. InGroup/InGroupSubtree).
    #[error("unsupported predicate: {kind}")]
    UnsupportedPredicate { kind: String },
}

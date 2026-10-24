//! The foundation's domain errors. `infra::error_mapping` is the single `DomainError` →
//! `CanonicalError` ladder (RFC 9457 on REST, `CanonicalError` on the SDK).

use std::fmt;
use std::sync::Arc;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::inbox::source::InboxSource;

/// Why a foundation operation did not complete.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// The object is absent or outside the caller's scope; the two are indistinguishable.
    #[error("{what} not found")]
    NotFound { what: &'static str },
    /// An inbox release named an entry that is not quarantined.
    #[error("inbox entry {entry_id} is not quarantined")]
    NotQuarantined { entry_id: Uuid },
    /// An inbox entry of a source no feature applies yet.
    #[error("no feature applies inbox source {inbox_source}")]
    SourceNotApplicable { inbox_source: InboxSource },
    /// The producer identity of an inbound fact does not verify for its source.
    #[error("the producer identity of inbox source {inbox_source} does not verify")]
    ProducerNotVerified { inbox_source: InboxSource },
    /// The owning tenant of an inbound payload is outside Rating's grant.
    #[error("tenant {tenant_id} is outside Rating's grant")]
    OutsideGrant { tenant_id: Uuid },
    /// The same `Idempotency-Key` arrived with a different request.
    #[error("idempotency key {key:?} was used with a different request")]
    IdempotencyKeyReused { key: String },
    /// A request value is malformed.
    #[error("invalid {field}: {detail}")]
    InvalidArgument {
        field: &'static str,
        detail: String,
        reason: &'static str,
    },
    /// A concurrent writer changed the row first (a failed CAS or a lost insert race).
    #[error("concurrent modification: {0}")]
    Concurrent(String),
    /// The PDP denied the operation or a selector lies outside the granted scope.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The PDP could not be evaluated; the PDP's error, when there is one, is the source.
    #[error("authorization unavailable: {0}")]
    AuthzUnavailable(String, #[source] Option<Cause>),
    /// The Cluster coordination backend could not be resolved or failed; the Cluster error, when
    /// there is one, is the source.
    #[error("coordination unavailable: {0}")]
    Coordination(String, #[source] Option<Cause>),
    /// A leader's work ended while its instance still led: it returned early or panicked.
    #[error("leader work ended: {0}")]
    LeaderWorkEnded(String),
    /// A stored row could not be read back into its domain type.
    #[error("corrupt row: {0}")]
    CorruptRow(String),
    /// The database failed; the detail is logged, never returned to a caller, and the storage
    /// error is the source.
    #[error("database error: {0}")]
    Database(String, #[source] Option<Cause>),
    /// A transaction could not begin or commit; the database error is the source.
    #[error("database transaction failed")]
    Transaction(#[source] Cause),
}

/// The failure behind an infrastructure error, kept whole as the error's source so a caller can
/// still tell a transient failure from a permanent one.
///
/// Shared so `DomainError` stays `Clone`; two causes are equal only when they are the same
/// failure.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Cause(Arc<dyn std::error::Error + Send + Sync + 'static>);

impl Cause {
    /// Keeps `err` as a cause.
    pub fn new(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(err))
    }

    /// The failure itself.
    #[must_use]
    pub fn get(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self.0.as_ref()
    }
}

impl PartialEq for Cause {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Cause {}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Cause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// `Db::transaction_ref_mapped` maps begin and commit failures into the closure's error type.
impl From<toolkit_db::DbError> for DomainError {
    fn from(err: toolkit_db::DbError) -> Self {
        Self::Transaction(Cause::new(err))
    }
}

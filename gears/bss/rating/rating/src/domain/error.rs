//! The foundation's domain errors. `infra::error_mapping` is the single `DomainError` →
//! `CanonicalError` ladder (RFC 9457 on REST, `CanonicalError` on the SDK).

use std::fmt;
use std::sync::Arc;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::authz::Operation;
use crate::domain::inbox::source::InboxSource;
use crate::domain::ports::{Dependency, FailureCode};

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
    /// The same `Idempotency-Key` arrived with a different request for `operation`, whose
    /// resource the refusal names.
    #[error("idempotency key {key:?} was used with a different request")]
    IdempotencyKeyReused { key: String, operation: Operation },
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
    /// A dependency other than the PDP and the Cluster backend could not answer (the usage feed,
    /// the meter declaration read); its error, when there is one, is the source. Transient: the
    /// caller backs off and changes nothing.
    #[error("{} unavailable: {detail}", .dependency.as_str())]
    DependencyUnavailable {
        dependency: Dependency,
        detail: String,
        #[source]
        cause: Option<Cause>,
    },
    /// A leader's work ended while its instance still led: it returned early or panicked; the
    /// task's `JoinError`, when there is one, is the source.
    #[error("leader work ended: {0}")]
    LeaderWorkEnded(String, #[source] Option<Cause>),
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
    /// A fault of Rating's own, neither the caller's nor a dependency's; retrying does not help.
    #[error("internal error: {0}")]
    Internal(String, #[source] Option<Cause>),
}

impl DomainError {
    /// The `(dependency, code)` sample of `rating_dependency_errors_total` this error reports, or
    /// `None` for an error that is the caller's, the stored data's or Rating's own. The one
    /// mapping every counting site uses, so a label never depends on where the error surfaced.
    #[must_use]
    pub const fn dependency_failure(&self) -> Option<(Dependency, FailureCode)> {
        match self {
            Self::AuthzUnavailable(..) => Some((Dependency::Pdp, FailureCode::EvaluationFailed)),
            Self::Coordination(..) => {
                Some((Dependency::Cluster, FailureCode::LeadershipUnavailable))
            }
            Self::Database(..) | Self::Transaction(_) => {
                Some((Dependency::Database, FailureCode::StorageFailed))
            }
            Self::DependencyUnavailable { dependency, .. } => {
                Some((*dependency, FailureCode::Unavailable))
            }
            Self::NotFound { .. }
            | Self::NotQuarantined { .. }
            | Self::SourceNotApplicable { .. }
            | Self::ProducerNotVerified { .. }
            | Self::OutsideGrant { .. }
            | Self::IdempotencyKeyReused { .. }
            | Self::InvalidArgument { .. }
            | Self::Concurrent(_)
            | Self::PermissionDenied(_)
            | Self::LeaderWorkEnded(..)
            | Self::CorruptRow(_)
            | Self::Internal(..) => None,
        }
    }
}

/// The failure behind an infrastructure error, kept whole as the error's source so a caller can
/// still tell a transient failure from a permanent one: [`Cause::get`] returns it.
///
/// Transparent in the source chain: it displays as the failure and continues the chain with the
/// failure's own source, so a chained report names the failure once.
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
        self.0.source()
    }
}

/// `Db::transaction_ref_mapped` maps begin and commit failures into the closure's error type.
impl From<toolkit_db::DbError> for DomainError {
    fn from(err: toolkit_db::DbError) -> Self {
        Self::Transaction(Cause::new(err))
    }
}

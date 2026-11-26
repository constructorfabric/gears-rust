//! Ports the resolver's logic depends on.

use toolkit_macros::domain_model;

/// The operation a call performed. A bounded set, because it is a metric label.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Debit,
    AcquireLease,
    CommitLease,
    ReleaseLease,
    ReturnAmount,
    QuotaState,
}

impl Operation {
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Debit => "debit",
            Self::AcquireLease => "acquire_lease",
            Self::CommitLease => "commit_lease",
            Self::ReleaseLease => "release_lease",
            Self::ReturnAmount => "return_amount",
            Self::QuotaState => "quota_state",
        }
    }
}

/// The contract a call came through.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    Platform,
    Tenant,
}

impl Plane {
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Tenant => "tenant",
        }
    }
}

/// How a call ended. A bounded set, because it is a metric label.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The call succeeded and, for a debit or acquisition, the backend allowed it.
    Allowed,
    /// The backend denied a debit or acquisition.
    Denied,
    /// A principal subject has no quota subject type; the backend was not called.
    PrincipalNotMapped,
    /// The call failed.
    Error,
}

impl Outcome {
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::PrincipalNotMapped => "principal_not_mapped",
            Self::Error => "error",
        }
    }
}

/// Sink for the resolver's metrics.
pub trait QuotaMetrics: Send + Sync {
    /// Count one finished call.
    fn record_operation(&self, operation: Operation, plane: Plane, outcome: Outcome);

    /// Record the time the resolver spent before it called the backend.
    fn record_overhead_ms(&self, millis: f64);
}

/// Metrics sink that drops everything.
#[domain_model]
pub struct NoopMetrics;

impl QuotaMetrics for NoopMetrics {
    fn record_operation(&self, _operation: Operation, _plane: Plane, _outcome: Outcome) {}

    fn record_overhead_ms(&self, _millis: f64) {}
}

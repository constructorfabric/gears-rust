//! Reconciliation outcomes, stop labels and availability classification.
//! Shared by execution, reports and telemetry without depending on the executor.

use std::collections::BTreeMap;

use gts::GtsId;
use toolkit_canonical_errors::CanonicalError;

/// Reconciliation stop reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconcileStop {
    /// No identifiers remain to submit.
    Completed,
    /// Pass limit reached with pending identifiers.
    PassesExhausted,
    /// Whole-call deadline reached.
    Deadline,
    /// Entity read failed.
    ReadFailed,
    /// A permanent submission refusal; no further batches or passes run.
    InvalidInput,
    /// A pass admitted nothing and only dependencies remain; see
    /// [`crate::ReconcileOptions::stop_when_stalled`].
    Stalled,
    /// A terminal item refusal prevents further passes; other batches of the pass ran.
    Rejected,
    /// A progress snapshot before the next pass; never a terminal stop.
    Retrying,
}

impl ReconcileStop {
    /// Parse a wire stop label; unknown labels return `None`.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "passes_exhausted" => Some(Self::PassesExhausted),
            "deadline" => Some(Self::Deadline),
            "read_failed" => Some(Self::ReadFailed),
            "invalid_input" => Some(Self::InvalidInput),
            "stalled" => Some(Self::Stalled),
            "rejected" => Some(Self::Rejected),
            "retrying" => Some(Self::Retrying),
            _ => None,
        }
    }

    /// Stable `reconcile_finished.stop` label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::PassesExhausted => "passes_exhausted",
            Self::Deadline => "deadline",
            Self::ReadFailed => "read_failed",
            Self::InvalidInput => "invalid_input",
            Self::Stalled => "stalled",
            Self::Rejected => "rejected",
            Self::Retrying => "retrying",
        }
    }
}

/// One desired identifier's outcome.
#[derive(Debug, Clone)]
pub enum ReconcileOutcome {
    /// Registered, or already present with the desired content.
    Admitted,
    /// Terminal refusal; decode registry reasons with [`crate::item_failure::AdmissionFailure::from_canonical`].
    Rejected(CanonicalError),
    /// Not settled by this call.
    Pending(ReconcilePendingCause),
}

/// Why an identifier is still pending.
#[derive(Debug, Clone)]
pub enum ReconcilePendingCause {
    /// Something it depends on is not registered yet.
    Dependency(CanonicalError),
    /// A concurrent writer changed it; the next pass re-reads.
    Conflict(CanonicalError),
    /// The registry could not be reached or did not answer in time.
    Unavailable(CanonicalError),
    /// Call-level refusal (e.g. auth/permission); this call does not submit it again, a
    /// later one may once its cause is fixed.
    Refused(CanonicalError),
    /// Unjudged after invalid input; used only when no earlier outcome is known.
    Skipped,
}

impl ReconcilePendingCause {
    /// Transport/availability failure maps to Unavailable; other call failures to Refused.
    pub(crate) fn of(error: CanonicalError) -> Self {
        if transport_failure(&error) {
            Self::Unavailable(error)
        } else {
            Self::Refused(error)
        }
    }
}

impl ReconcileOutcome {
    /// Whether another call could change this outcome.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Pending(_))
    }
}

/// Outcomes so far, by canonical identifier.
pub type Outcomes = BTreeMap<String, (GtsId, ReconcileOutcome)>;

/// Transport/registry availability failures retain [`ReconcilePendingCause::Unavailable`].
pub(super) fn transport_failure(error: &CanonicalError) -> bool {
    retryable(error)
        || matches!(
            error,
            CanonicalError::DeadlineExceeded { .. } | CanonicalError::ResourceExhausted { .. }
        )
}

/// Transport failures worth repeating under the same key and request.
pub(super) fn retryable(error: &CanonicalError) -> bool {
    matches!(
        error,
        CanonicalError::Aborted { .. }
            | CanonicalError::ServiceUnavailable { .. }
            | CanonicalError::Internal { .. }
            | CanonicalError::Unknown { .. }
    )
}

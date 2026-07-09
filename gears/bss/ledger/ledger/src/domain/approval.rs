//! Dual-control governance domain (VHP-1852): the approval `kind` discriminator,
//! the `PENDING → APPROVED | REJECTED | NEEDS_REWORK | CANCELLED | EXPIRED` state
//! type, and the pure threshold-policy resolver ([`policy`]). The state-machine
//! transition rules and the `preparer ≠ approver` enforcement live with the
//! `ApprovalService` (infra, Group D); this module holds the pure types + the
//! threshold logic so they are unit-testable without a database.

use toolkit_macros::domain_model;

pub mod intent;
pub mod policy;

/// Which governed mutation an approval gates. Stamped (as [`Self::as_str`]) into
/// `ledger_approval.kind`; the same set is the DB CHECK in migration `000012`.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    Reverse,
    MaterialBackdating,
    CreditGrant,
    ChargebackLoss,
    PayerClosure,
    PeriodReopen,
    RecognitionScheduleChange,
    Refund,
    ManualAdjustment,
    CreditNote,
    DebitNote,
}

impl ApprovalKind {
    /// The stable wire/DB token. Inverse of [`Self::parse`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reverse => "REVERSE",
            Self::MaterialBackdating => "MATERIAL_BACKDATING",
            Self::CreditGrant => "CREDIT_GRANT",
            Self::ChargebackLoss => "CHARGEBACK_LOSS",
            Self::PayerClosure => "PAYER_CLOSURE",
            Self::PeriodReopen => "PERIOD_REOPEN",
            Self::RecognitionScheduleChange => "RECOGNITION_SCHEDULE_CHANGE",
            Self::Refund => "REFUND",
            Self::ManualAdjustment => "MANUAL_ADJUSTMENT",
            Self::CreditNote => "CREDIT_NOTE",
            Self::DebitNote => "DEBIT_NOTE",
        }
    }

    /// Parse a stored token back into a kind — the inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "REVERSE" => Some(Self::Reverse),
            "MATERIAL_BACKDATING" => Some(Self::MaterialBackdating),
            "CREDIT_GRANT" => Some(Self::CreditGrant),
            "CHARGEBACK_LOSS" => Some(Self::ChargebackLoss),
            "PAYER_CLOSURE" => Some(Self::PayerClosure),
            "PERIOD_REOPEN" => Some(Self::PeriodReopen),
            "RECOGNITION_SCHEDULE_CHANGE" => Some(Self::RecognitionScheduleChange),
            "REFUND" => Some(Self::Refund),
            "MANUAL_ADJUSTMENT" => Some(Self::ManualAdjustment),
            "CREDIT_NOTE" => Some(Self::CreditNote),
            "DEBIT_NOTE" => Some(Self::DebitNote),
            _ => None,
        }
    }
}

/// The lifecycle state of a `ledger_approval` row. `PENDING`/`NEEDS_REWORK` are
/// the active states (idempotency + expiry apply to them); `APPROVING` is the
/// transient execute-then-mark latch (H2 — claimed before the mutation runs so a
/// concurrent reject/cancel/request-changes can no longer win, never released
/// early, never expired mid-flight); the rest are terminal.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    /// Transient: an approve has been authorized and is executing the mutation;
    /// only the approve flow may move it on (→ `Approved`, or back to `Pending`
    /// if the mutation failed without committing). Not cancellable/rejectable.
    Approving,
    Approved,
    Rejected,
    NeedsRework,
    Cancelled,
    Expired,
}

impl ApprovalState {
    /// The stable wire/DB token. Inverse of [`Self::parse`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Approving => "APPROVING",
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
            Self::NeedsRework => "NEEDS_REWORK",
            Self::Cancelled => "CANCELLED",
            Self::Expired => "EXPIRED",
        }
    }

    /// Parse a stored token back into a state — the inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "PENDING" => Some(Self::Pending),
            "APPROVING" => Some(Self::Approving),
            "APPROVED" => Some(Self::Approved),
            "REJECTED" => Some(Self::Rejected),
            "NEEDS_REWORK" => Some(Self::NeedsRework),
            "CANCELLED" => Some(Self::Cancelled),
            "EXPIRED" => Some(Self::Expired),
            _ => None,
        }
    }

    /// The active states — the ones that still move through the workflow
    /// (idempotency + expiry apply): `PENDING` and `NEEDS_REWORK`. Mirrors
    /// [`Self::is_active`]; used to build the SQL `state IN (…)` predicate for the
    /// active set (`.map(Self::as_str)`). Note the idempotency lookup
    /// ([`ApprovalRepo::read_active`](crate::infra::storage::repo::ApprovalRepo))
    /// additionally counts the transient `APPROVING` latch as active — that is a
    /// DIFFERENT, wider set and is spelled out explicitly there.
    pub const ACTIVE: [ApprovalState; 2] = [Self::Pending, Self::NeedsRework];

    /// An active state still moves through the workflow (idempotency + expiry
    /// apply); a terminal state never transitions again.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::NeedsRework)
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;

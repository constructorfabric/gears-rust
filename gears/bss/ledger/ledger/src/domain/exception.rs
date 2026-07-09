//! Exception-queue taxonomy (Slice 7, design §4.6 / §7).
//!
//! `ExceptionType` is the closed set of durable close-blocking exception kinds
//! (the `ledger_exception_queue.exception_type` CHECK), and `ExceptionStatus` is
//! the resolution lifecycle (`OPEN → ACK → RESOLVED`, or `OPEN → APPROVED_EXCEPTION`
//! for the one acknowledge-to-non-block `GL_WRITEOFF_VARIANCE`). Both are state
//! machines with a persisted wire contract, so they are enums (the
//! [`crate::domain::status`] convention) — the `as_str` token is the stored value,
//! the values never change.

use std::fmt;

use toolkit_macros::domain_model;

/// One durable exception-queue kind (design §4.6 / §7). Every kind blocks period
/// close while `OPEN`; `GL_WRITEOFF_VARIANCE` is the one Finance can acknowledge to
/// `APPROVED_EXCEPTION` (a knowingly-accumulating BSS↔ERP AR overstatement), after
/// which it no longer blocks (N-pay-5). `as_str` is the stored / wire token.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceptionType {
    /// A settled payment with no allocation match (Slice 2).
    SettledNoMatch,
    /// A Slice 1 `mapping_status = PENDING` suspense line.
    MappingGap,
    /// A reconciliation variance / a clawback that never matched an outbound
    /// refund (the Slice 3 `CLAWBACK_UNDERFLOW` escalation).
    ReconMismatch,
    /// A Payments↔PSP settlement variance (Phase 3).
    PspVariance,
    /// A credit note whose recognized-vs-deferred split basis is ambiguous
    /// (Slice 3 §4.2 / `CreditNoteSplitAmbiguous`).
    SplitAmbiguous,
    /// A recognition policy conflict (e.g. a debit-note schedule's catalog-vs-
    /// contract ambiguity, `RecognitionPolicyConflict`).
    RecognitionPolicyConflict,
    /// An unscheduled deferral (Slice 4 §4.2). Reserved — no upstream stub
    /// materialises it yet.
    UnscheduledDeferral,
    /// A stage-1 `REFUND_CLEARING` unrelieved past the 14-day page threshold
    /// (Slice 3 aging / Slice 7 PSP tie, §4.6).
    StuckRefundClearing,
    /// A settlement return that cannot fit under the money-out cap (Slice 2
    /// §4.2, `SettlementReturnOverAllocated`).
    SettlementReturnOverAllocated,
    /// A chargeback `lost` clawback on an already-refunded payment (Slice 2 §4.5,
    /// `ChargebackOnRefunded`).
    ChargebackOnRefunded,
    /// A GL-side write-off variance — the one acknowledge-to-non-block kind
    /// (N-pay-5): Finance approves it to `APPROVED_EXCEPTION` and it no longer
    /// blocks close (a tracked, accumulating BSS↔ERP AR overstatement).
    GlWriteoffVariance,
    /// An issued invoice in the upstream manifest with no committed `INVOICE_POST`
    /// (Rev3 / N-recon-1; Phase 3 invoice-completeness check).
    MissedPosting,
}

impl ExceptionType {
    /// The stored / wire token (the `exception_type` column value). MUST match the
    /// migration CHECK and never change.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SettledNoMatch => "SETTLED_NO_MATCH",
            Self::MappingGap => "MAPPING_GAP",
            Self::ReconMismatch => "RECON_MISMATCH",
            Self::PspVariance => "PSP_VARIANCE",
            Self::SplitAmbiguous => "SPLIT_AMBIGUOUS",
            Self::RecognitionPolicyConflict => "RECOGNITION_POLICY_CONFLICT",
            Self::UnscheduledDeferral => "UNSCHEDULED_DEFERRAL",
            Self::StuckRefundClearing => "STUCK_REFUND_CLEARING",
            Self::SettlementReturnOverAllocated => "SETTLEMENT_RETURN_OVER_ALLOCATED",
            Self::ChargebackOnRefunded => "CHARGEBACK_ON_REFUNDED",
            Self::GlWriteoffVariance => "GL_WRITEOFF_VARIANCE",
            Self::MissedPosting => "MISSED_POSTING",
        }
    }

    /// Parse a stored token back to the enum (`None` on an unknown literal).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "SETTLED_NO_MATCH" => Self::SettledNoMatch,
            "MAPPING_GAP" => Self::MappingGap,
            "RECON_MISMATCH" => Self::ReconMismatch,
            "PSP_VARIANCE" => Self::PspVariance,
            "SPLIT_AMBIGUOUS" => Self::SplitAmbiguous,
            "RECOGNITION_POLICY_CONFLICT" => Self::RecognitionPolicyConflict,
            "UNSCHEDULED_DEFERRAL" => Self::UnscheduledDeferral,
            "STUCK_REFUND_CLEARING" => Self::StuckRefundClearing,
            "SETTLEMENT_RETURN_OVER_ALLOCATED" => Self::SettlementReturnOverAllocated,
            "CHARGEBACK_ON_REFUNDED" => Self::ChargebackOnRefunded,
            "GL_WRITEOFF_VARIANCE" => Self::GlWriteoffVariance,
            "MISSED_POSTING" => Self::MissedPosting,
            _ => return None,
        })
    }
}

impl fmt::Display for ExceptionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The resolution lifecycle of an exception row (design §4.6). An `OPEN` row
/// blocks close; `ACK`/`RESOLVED`/`APPROVED_EXCEPTION` do not. The transitions:
/// `OPEN → ACK → RESOLVED` (operator triage) or `OPEN → APPROVED_EXCEPTION`
/// (Finance, `GL_WRITEOFF_VARIANCE` only).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExceptionStatus {
    /// Open — blocks period close until resolved or approved.
    Open,
    /// Operator-acknowledged (triage started); no longer blocks close.
    Ack,
    /// Resolved (the underlying condition cleared).
    Resolved,
    /// A Finance-approved standing exception (`GL_WRITEOFF_VARIANCE` only).
    ApprovedException,
}

impl ExceptionStatus {
    /// The stored / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Ack => "ACK",
            Self::Resolved => "RESOLVED",
            Self::ApprovedException => "APPROVED_EXCEPTION",
        }
    }

    /// Parse a stored token (`None` on an unknown literal).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "OPEN" => Self::Open,
            "ACK" => Self::Ack,
            "RESOLVED" => Self::Resolved,
            "APPROVED_EXCEPTION" => Self::ApprovedException,
            _ => return None,
        })
    }

    /// Whether an exception in this status blocks period close. Only `OPEN` does;
    /// every resolution state (incl. the Finance-approved GL-writeoff) clears the
    /// block (mirrors the close gate's `list_open_in_txn`, which filters `OPEN`).
    #[must_use]
    pub const fn blocks_close(self) -> bool {
        matches!(self, Self::Open)
    }
}

impl fmt::Display for ExceptionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;

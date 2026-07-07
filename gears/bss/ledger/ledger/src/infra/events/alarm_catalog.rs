//! The normative §4.7 alarm catalog over [`AlarmCategory`].
//!
//! Three pure lookups translate an [`AlarmCategory`] into the design's §4.7
//! columns:
//! - [`severity`] — the §4.7 "Severity" column (`Warn` / `Critical`).
//! - [`routing`] — a short, stable string of the §4.7 "Required behavior" /
//!   route (e.g. who to page, what to block). Consumers MUST treat this as a
//!   human-facing label, not a parseable contract.
//! - [`owning_slice`] — which architecture slice owns the row.
//!
//! All three are **exhaustive** `match`es on [`AlarmCategory`]. That is the
//! point: adding a variant to the enum forces an entry in each of these
//! functions (the compiler refuses a non-exhaustive `match`), so the catalog
//! can never silently fall out of step with the enum.
//!
//! This module is data-only: it does not emit, route, or page. The emitters
//! (the posting service's `alarm_for`, the tie-out job, the chain Verifier) own
//! WHEN an alarm fires; this catalog owns its severity + route metadata.

use crate::infra::events::payloads::{AlarmCategory, AlarmSeverity};

/// The §4.7 "Severity" column for `cat`.
///
/// `Critical` rows are the integrity-/cash-threatening defects that page
/// immediately: zero-sum (`EntryImbalance`), the negative-balance NO-class
/// (`NegativeBalanceViolation`), `RecognitionDoubleCredit`, `OverRecognition`,
/// `FxSnapshotMissing`, `FxSnapshotStaleBlocked`, the idempotency collision
/// (`IdempotencyPayloadConflict`), `NegativeTaxSubbalance`, the tamper failure
/// (`TamperVerifyFailed`), `AttemptedWriteOff`, and `FxRevaluationIncomplete`.
/// Every other row is `Warn` (some are "Warn→Page" in the design — still a
/// `Warn` severity here; escalation is the consumer's routing concern).
#[must_use]
pub fn severity(cat: AlarmCategory) -> AlarmSeverity {
    use AlarmCategory as C;
    use AlarmSeverity::{Critical, Warn};
    match cat {
        // ── Critical (§4.7 Severity = Critical) ──────────────────────────────
        C::EntryImbalance
        | C::NegativeBalanceViolation
        | C::RecognitionDoubleCredit
        | C::OverRecognition
        | C::FxSnapshotMissing
        | C::FxSnapshotStaleBlocked
        | C::IdempotencyPayloadConflict
        | C::NegativeTaxSubbalance
        | C::TamperVerifyFailed
        | C::AttemptedWriteOff
        | C::ClawbackUnderflow
        | C::StuckRefundClearing
        | C::FxRevaluationIncomplete => Critical,
        // ── Warn (everything else, incl. the "Warn→Page" rows) ───────────────
        C::TieOutVariance
        | C::ChargebackCashNegative
        | C::FxSnapshotStaleAllowed
        | C::CreditNoteSplitBlocked
        | C::RefundQuarantined
        | C::RecognitionPeriodQueued
        | C::ReconciliationVariance
        | C::ExportFailedAged
        | C::AgedAllocationQueue
        | C::AgedUnallocated
        | C::RefundClearingAged
        | C::DisputePhaseQueued
        | C::Stage1RefundOrphan
        | C::BillrunPartialFailure
        | C::PartitionDetachBlocked
        | C::RelayLag
        | C::ClockSkew
        | C::PayerAttributionDrift
        | C::DormantOpenCredit
        | C::ChainLag
        | C::SubtreeTooLarge
        | C::SubtreeResolutionDegraded
        | C::MissedPosting => Warn,
    }
}

/// The §4.7 "Required behavior" / route for `cat`, as a short stable label.
///
/// Human-facing only (a page/route hint for operators); never parse it. Every
/// arm returns a non-empty string — the `alarm_catalog_tests` enforce that, so
/// a new variant cannot ship with an empty route.
#[must_use]
#[allow(
    clippy::match_same_arms,
    reason = "the catalog names each category explicitly for traceability; distinct categories may legitimately share a route/slice"
)]
pub fn routing(cat: AlarmCategory) -> &'static str {
    use AlarmCategory as C;
    match cat {
        C::IdempotencyPayloadConflict => "Reject the conflicting post; page Revenue Assurance",
        C::NegativeBalanceViolation => "Route to Revenue Assurance",
        C::TieOutVariance => "Route to Revenue Assurance; block period close above tolerance",
        C::EntryImbalance => "Reject the entry; page Revenue Assurance",
        C::TamperVerifyFailed => "Freeze write path on scope; page Audit + Architecture",
        C::ChargebackCashNegative => "Route to Revenue Assurance; review chargeback handling",
        C::RecognitionDoubleCredit => "Block recognition; page Revenue Assurance",
        C::OverRecognition => "Block recognition; page Revenue Assurance",
        C::FxSnapshotMissing => "Block FX-dependent post; page Revenue Assurance",
        C::FxSnapshotStaleAllowed => "Allow within tolerance; record warning for Revenue Assurance",
        C::FxSnapshotStaleBlocked => "Block FX-dependent post; page Revenue Assurance",
        C::NegativeTaxSubbalance => "Block; page Tax + Revenue Assurance",
        C::CreditNoteSplitBlocked => "Block credit-note split; queue for Billing review",
        C::RefundQuarantined => "Quarantine refund; queue for Billing review",
        C::RecognitionPeriodQueued => "Queue for the recognition period sweep",
        C::ReconciliationVariance => "Block period close above tolerance; route to Reconciliation",
        C::ExportFailedAged => "Retry export; escalate to Reconciliation when aged",
        C::AgedAllocationQueue => "Queue for allocation; escalate when aged",
        C::AgedUnallocated => "Queue for allocation; escalate to Billing when aged",
        C::RefundClearingAged => "Escalate aged refund-clearing balance to Billing",
        C::DisputePhaseQueued => "Queue for the dispute-phase sweep",
        C::Stage1RefundOrphan => "Queue orphaned stage-1 refund for Billing review",
        C::BillrunPartialFailure => "Retry the failed bill-run shard; page Billing on repeat",
        C::PartitionDetachBlocked => "Block partition detach; route to Platform Operations",
        C::RelayLag => "Monitor relay backlog; page Platform Operations when sustained",
        C::ClockSkew => "Quarantine the skewed post; route to Platform Operations",
        C::AttemptedWriteOff => "Reject the write-off; page Revenue Assurance + Audit",
        C::ClawbackUnderflow => "Reconcile the unmatched refund claw-back; page Finance",
        C::StuckRefundClearing => {
            "Block period close; escalate the stuck refund-clearing balance to Billing + Reconciliation"
        }
        C::PayerAttributionDrift => {
            "INACTIVE in MVP (detective seam, no-op until the AuthZ/Tenant resolver is wired — \
             design F-9); when active: route to Revenue Assurance, review payer attribution"
        }
        C::FxRevaluationIncomplete => "Block period close; page Revenue Assurance",
        C::DormantOpenCredit => "Queue dormant open credit for Billing review",
        C::ChainLag => "Monitor chain-seal backlog; page Platform Operations when sustained",
        C::SubtreeTooLarge => "Route to Platform Operations; review tenant subtree size",
        C::SubtreeResolutionDegraded => "Route to Platform Operations; review resolver health",
        C::MissedPosting => {
            "Block period close; page Reconciliation on a missed posting aged past threshold"
        }
    }
}

/// The architecture slice that owns `cat` (e.g. `"Slice 6"`).
#[must_use]
pub fn owning_slice(cat: AlarmCategory) -> &'static str {
    use AlarmCategory as C;
    match cat {
        // Slice 1 — core posting / structural invariants + tenant resolution.
        C::ClockSkew
        | C::PayerAttributionDrift
        | C::SubtreeTooLarge
        | C::SubtreeResolutionDegraded => "Slice 1",
        // Slice 2 — bill-run / invoicing.
        C::BillrunPartialFailure => "Slice 2",
        // Slice 3 — revenue recognition + FX.
        C::RecognitionDoubleCredit
        | C::OverRecognition
        | C::FxSnapshotMissing
        | C::FxSnapshotStaleAllowed
        | C::FxSnapshotStaleBlocked
        | C::RecognitionPeriodQueued
        | C::FxRevaluationIncomplete => "Slice 3",
        // Slice 4 — credits / refunds / disputes / write-offs.
        C::ChargebackCashNegative
        | C::CreditNoteSplitBlocked
        | C::RefundQuarantined
        | C::AgedAllocationQueue
        | C::AgedUnallocated
        | C::RefundClearingAged
        | C::DisputePhaseQueued
        | C::Stage1RefundOrphan
        | C::AttemptedWriteOff
        | C::ClawbackUnderflow
        | C::StuckRefundClearing
        | C::DormantOpenCredit => "Slice 4",
        // Slice 5 — tax.
        C::NegativeTaxSubbalance => "Slice 5",
        // Slice 6 — ledger integrity / tamper chain / event relay.
        C::IdempotencyPayloadConflict
        | C::NegativeBalanceViolation
        | C::EntryImbalance
        | C::TamperVerifyFailed
        | C::RelayLag
        | C::ChainLag => "Slice 6",
        // Slice 7 — reconciliation / export / period close / partitions.
        C::TieOutVariance
        | C::ReconciliationVariance
        | C::ExportFailedAged
        | C::PartitionDetachBlocked
        | C::MissedPosting => "Slice 7",
    }
}

#[cfg(test)]
#[path = "alarm_catalog_tests.rs"]
mod alarm_catalog_tests;

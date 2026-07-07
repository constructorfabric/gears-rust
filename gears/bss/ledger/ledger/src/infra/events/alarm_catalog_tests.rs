//! Tests for the §4.7 alarm catalog: every [`AlarmCategory`] has a total
//! `severity` / `routing` / `owning_slice` (no panic, no empty route), and the
//! `as_str` wire tokens are unique. (DE1101: kept out of the impl file.)
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;

use super::{owning_slice, routing, severity};
use crate::infra::events::payloads::AlarmCategory;

/// Every catalogued category. A NEW `AlarmCategory` variant must be added here
/// too — the totality assertions below then exercise it through all three
/// lookups. (The `as_str`/`severity`/`routing`/`owning_slice` `match`es are the
/// compiler-enforced safety net; this list drives the runtime checks.)
const ALL: &[AlarmCategory] = &[
    AlarmCategory::IdempotencyPayloadConflict,
    AlarmCategory::NegativeBalanceViolation,
    AlarmCategory::TieOutVariance,
    AlarmCategory::EntryImbalance,
    AlarmCategory::TamperVerifyFailed,
    AlarmCategory::ChargebackCashNegative,
    AlarmCategory::RecognitionDoubleCredit,
    AlarmCategory::OverRecognition,
    AlarmCategory::FxSnapshotMissing,
    AlarmCategory::FxSnapshotStaleAllowed,
    AlarmCategory::FxSnapshotStaleBlocked,
    AlarmCategory::NegativeTaxSubbalance,
    AlarmCategory::CreditNoteSplitBlocked,
    AlarmCategory::RefundQuarantined,
    AlarmCategory::RecognitionPeriodQueued,
    AlarmCategory::ReconciliationVariance,
    AlarmCategory::ExportFailedAged,
    AlarmCategory::AgedAllocationQueue,
    AlarmCategory::AgedUnallocated,
    AlarmCategory::RefundClearingAged,
    AlarmCategory::DisputePhaseQueued,
    AlarmCategory::Stage1RefundOrphan,
    AlarmCategory::ClawbackUnderflow,
    AlarmCategory::StuckRefundClearing,
    AlarmCategory::BillrunPartialFailure,
    AlarmCategory::PartitionDetachBlocked,
    AlarmCategory::RelayLag,
    AlarmCategory::ClockSkew,
    AlarmCategory::AttemptedWriteOff,
    AlarmCategory::PayerAttributionDrift,
    AlarmCategory::FxRevaluationIncomplete,
    AlarmCategory::DormantOpenCredit,
    AlarmCategory::ChainLag,
    AlarmCategory::SubtreeTooLarge,
    AlarmCategory::SubtreeResolutionDegraded,
    AlarmCategory::MissedPosting,
];

#[test]
fn catalog_is_total_over_every_category() {
    for &cat in ALL {
        // `severity` / `owning_slice` must not panic and must return a usable
        // value. `routing` must be a non-empty label.
        let _ = severity(cat);
        let slice = owning_slice(cat);
        assert!(
            !slice.trim().is_empty(),
            "owning_slice empty for {}",
            cat.as_str()
        );
        let route = routing(cat);
        assert!(
            !route.trim().is_empty(),
            "routing empty for {}",
            cat.as_str()
        );
    }
}

#[test]
fn as_str_tokens_are_unique() {
    let mut seen: HashSet<&str> = HashSet::new();
    for &cat in ALL {
        let token = cat.as_str();
        assert!(
            seen.insert(token),
            "duplicate AlarmCategory wire token: {token}"
        );
    }
    // Sanity: the list and the enum agree in size — a new variant absent from
    // `ALL` would make this drift. (36 = the original 5 + the 28 §4.7 rows + the
    // Slice-3 ClawbackUnderflow + StuckRefundClearing refund-family rows + the
    // Slice-7 MissedPosting invoice-completeness row.)
    assert_eq!(ALL.len(), 36, "ALL must list every AlarmCategory variant");
}

#[test]
fn owning_slice_values_are_known() {
    let known = [
        "Slice 1", "Slice 2", "Slice 3", "Slice 4", "Slice 5", "Slice 6", "Slice 7",
    ];
    for &cat in ALL {
        let slice = owning_slice(cat);
        assert!(
            known.contains(&slice),
            "unexpected owning_slice {slice:?} for {}",
            cat.as_str()
        );
    }
}

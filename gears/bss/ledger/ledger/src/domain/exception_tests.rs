//! Tests for the exception-queue taxonomy enums.

#![allow(clippy::unwrap_used)]

use super::{ExceptionStatus, ExceptionType};

/// Every `ExceptionType` round-trips through its stored token.
#[test]
fn exception_type_round_trips() {
    let all = [
        ExceptionType::SettledNoMatch,
        ExceptionType::MappingGap,
        ExceptionType::ReconMismatch,
        ExceptionType::PspVariance,
        ExceptionType::SplitAmbiguous,
        ExceptionType::RecognitionPolicyConflict,
        ExceptionType::UnscheduledDeferral,
        ExceptionType::StuckRefundClearing,
        ExceptionType::SettlementReturnOverAllocated,
        ExceptionType::ChargebackOnRefunded,
        ExceptionType::GlWriteoffVariance,
        ExceptionType::MissedPosting,
    ];
    for ty in all {
        assert_eq!(
            ExceptionType::parse(ty.as_str()),
            Some(ty),
            "{ty} round-trip"
        );
    }
    assert_eq!(ExceptionType::parse("NOPE"), None);
}

/// The tokens match the migration CHECK literals exactly (a drift would make a
/// routed row fail the constraint at insert).
#[test]
fn exception_type_tokens_are_stable() {
    assert_eq!(ExceptionType::ReconMismatch.as_str(), "RECON_MISMATCH");
    assert_eq!(ExceptionType::SplitAmbiguous.as_str(), "SPLIT_AMBIGUOUS");
    assert_eq!(
        ExceptionType::ChargebackOnRefunded.as_str(),
        "CHARGEBACK_ON_REFUNDED"
    );
    assert_eq!(
        ExceptionType::SettlementReturnOverAllocated.as_str(),
        "SETTLEMENT_RETURN_OVER_ALLOCATED"
    );
    assert_eq!(
        ExceptionType::StuckRefundClearing.as_str(),
        "STUCK_REFUND_CLEARING"
    );
    assert_eq!(
        ExceptionType::GlWriteoffVariance.as_str(),
        "GL_WRITEOFF_VARIANCE"
    );
}

/// Only `OPEN` blocks close; every resolution state (incl. the Finance-approved
/// GL-writeoff) clears the block.
#[test]
fn only_open_blocks_close() {
    assert!(ExceptionStatus::Open.blocks_close());
    assert!(!ExceptionStatus::Ack.blocks_close());
    assert!(!ExceptionStatus::Resolved.blocks_close());
    assert!(!ExceptionStatus::ApprovedException.blocks_close());
}

/// `ExceptionStatus` round-trips through its stored token.
#[test]
fn exception_status_round_trips() {
    for st in [
        ExceptionStatus::Open,
        ExceptionStatus::Ack,
        ExceptionStatus::Resolved,
        ExceptionStatus::ApprovedException,
    ] {
        assert_eq!(ExceptionStatus::parse(st.as_str()), Some(st));
    }
    assert_eq!(ExceptionStatus::parse("nope"), None);
}

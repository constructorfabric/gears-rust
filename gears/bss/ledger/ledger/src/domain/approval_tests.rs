//! Unit tests for the dual-control `ApprovalKind` / `ApprovalState` tokens.

use super::*;

#[test]
fn approval_kind_token_roundtrips() {
    for k in [
        ApprovalKind::Reverse,
        ApprovalKind::MaterialBackdating,
        ApprovalKind::CreditGrant,
        ApprovalKind::ChargebackLoss,
        ApprovalKind::PayerClosure,
        ApprovalKind::PeriodReopen,
        ApprovalKind::RecognitionScheduleChange,
        ApprovalKind::Refund,
        ApprovalKind::ManualAdjustment,
    ] {
        assert_eq!(ApprovalKind::parse(k.as_str()), Some(k), "roundtrip {k:?}");
    }
    assert_eq!(ApprovalKind::parse("NOT_A_KIND"), None);
}

#[test]
fn approval_state_token_roundtrips() {
    for s in [
        ApprovalState::Pending,
        ApprovalState::Approving,
        ApprovalState::Approved,
        ApprovalState::Rejected,
        ApprovalState::NeedsRework,
        ApprovalState::Cancelled,
        ApprovalState::Expired,
    ] {
        assert_eq!(ApprovalState::parse(s.as_str()), Some(s), "roundtrip {s:?}");
    }
    assert_eq!(ApprovalState::parse("NOT_A_STATE"), None);
}

#[test]
fn only_pending_and_needs_rework_are_active() {
    assert!(ApprovalState::Pending.is_active());
    assert!(ApprovalState::NeedsRework.is_active());
    // `APPROVING` is transient (the H2 execute latch), NOT active: it is not
    // cancellable/rejectable — only the approve flow may move it on.
    assert!(
        !ApprovalState::Approving.is_active(),
        "APPROVING is transient, not active"
    );
    for terminal in [
        ApprovalState::Approved,
        ApprovalState::Rejected,
        ApprovalState::Cancelled,
        ApprovalState::Expired,
    ] {
        assert!(!terminal.is_active(), "{terminal:?} must be terminal");
    }
}

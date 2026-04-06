use super::*;

fn input(state: TurnState, error_code: Option<&str>, has_usage: bool) -> BillingDerivationInput {
    BillingDerivationInput {
        terminal_state: state,
        error_code: error_code.map(String::from),
        has_usage,
    }
}

// ── Completed ──

#[test]
fn completed_derives_completed_actual() {
    let r = derive_billing_outcome(&input(TurnState::Completed, None, true));
    assert_eq!(r.outcome, BillingOutcome::Completed);
    assert_eq!(r.settlement_method, SettlementMethod::Actual);
    assert!(!r.unknown_error_code);
}

// ── Cancelled ──

#[test]
fn cancelled_with_usage_derives_aborted_actual() {
    let r = derive_billing_outcome(&input(TurnState::Cancelled, None, true));
    assert_eq!(r.outcome, BillingOutcome::Aborted);
    assert_eq!(r.settlement_method, SettlementMethod::Actual);
}

#[test]
fn cancelled_without_usage_derives_aborted_estimated() {
    let r = derive_billing_outcome(&input(TurnState::Cancelled, None, false));
    assert_eq!(r.outcome, BillingOutcome::Aborted);
    assert_eq!(r.settlement_method, SettlementMethod::Estimated);
}

// ── Failed: orphan_timeout ──

#[test]
fn orphan_timeout_derives_aborted_estimated() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("orphan_timeout"), true));
    assert_eq!(r.outcome, BillingOutcome::Aborted);
    assert_eq!(r.settlement_method, SettlementMethod::Estimated);
    // Usage is ignored for orphan_timeout — always estimated.
}

// ── Failed: pre-provider errors ──

#[test]
fn context_length_exceeded_derives_failed_released() {
    let r = derive_billing_outcome(&input(
        TurnState::Failed,
        Some("context_length_exceeded"),
        false,
    ));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Released);
}

#[test]
fn validation_error_derives_failed_released() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("validation_error"), false));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Released);
}

// ── Failed: post-provider errors ──

#[test]
fn provider_error_with_usage_derives_failed_actual() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("provider_error"), true));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Actual);
}

#[test]
fn provider_error_without_usage_derives_failed_estimated() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("provider_error"), false));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Estimated);
}

#[test]
fn rate_limited_with_usage_derives_failed_actual() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("rate_limited"), true));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Actual);
}

// ── Failed: unknown error code ──

#[test]
fn unknown_error_code_derives_failed_estimated_with_flag() {
    let r = derive_billing_outcome(&input(TurnState::Failed, Some("some_new_code"), true));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Estimated);
    assert!(r.unknown_error_code);
}

#[test]
fn failed_with_no_error_code_derives_failed_estimated_with_flag() {
    let r = derive_billing_outcome(&input(TurnState::Failed, None, false));
    assert_eq!(r.outcome, BillingOutcome::Failed);
    assert_eq!(r.settlement_method, SettlementMethod::Estimated);
    assert!(r.unknown_error_code);
}

// ── Edge case: Running state ──

#[test]
#[should_panic(expected = "finalization called with Running state")]
#[allow(clippy::let_underscore_must_use, dropping_copy_types)]
fn running_state_panics() {
    // The function panics before returning, so the result is never used.
    drop(derive_billing_outcome(&input(
        TurnState::Running,
        None,
        false,
    )));
}

use modkit_macros::domain_model;

use crate::domain::model::quota::SettlementMethod;
use crate::infra::db::entity::chat_turn::TurnState;

/// Billing outcome classification for a finalized turn.
///
/// Maps from `TurnState` but is NOT a 1:1 mapping — see
/// DESIGN.md §5.8 "Critical Distinction: Internal State vs Billing Outcome".
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingOutcome {
    Completed,
    Failed,
    Aborted,
}

impl BillingOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }
}

/// Result of billing outcome derivation.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingDerivation {
    pub outcome: BillingOutcome,
    pub settlement_method: SettlementMethod,
    /// When `true`, the caller MUST log a critical error and increment
    /// `mini_chat_unknown_error_code_total` after the transaction commits.
    /// Kept out of the pure function to preserve testability.
    pub unknown_error_code: bool,
}

/// Input to `derive_billing_outcome()`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct BillingDerivationInput {
    pub terminal_state: TurnState,
    pub error_code: Option<String>,
    /// `true` when the provider reported a `Usage` object with at least one
    /// non-zero field (`input_tokens > 0 || output_tokens > 0`).
    /// A zero-valued or missing usage object is "usage unknown".
    pub has_usage: bool,
}

/// Derive billing outcome from terminal condition.
///
/// Pure function — no I/O, no logging, no metrics. The `unknown_error_code`
/// flag on the result signals the caller to emit side effects after the
/// transaction commits.
///
/// Implements the normative mapping from DESIGN.md §5.8.
#[must_use]
pub fn derive_billing_outcome(input: &BillingDerivationInput) -> BillingDerivation {
    match input.terminal_state {
        TurnState::Completed => BillingDerivation {
            outcome: BillingOutcome::Completed,
            settlement_method: SettlementMethod::Actual,
            unknown_error_code: false,
        },

        TurnState::Cancelled => {
            // Client disconnect — billing ABORTED.
            // Settlement depends on whether the provider reported usage.
            if input.has_usage {
                BillingDerivation {
                    outcome: BillingOutcome::Aborted,
                    settlement_method: SettlementMethod::Actual,
                    unknown_error_code: false,
                }
            } else {
                BillingDerivation {
                    outcome: BillingOutcome::Aborted,
                    settlement_method: SettlementMethod::Estimated,
                    unknown_error_code: false,
                }
            }
        }

        TurnState::Failed => match input.error_code.as_deref() {
            // Orphan timeout — always ABORTED/Estimated (no provider terminal event).
            Some("orphan_timeout") => BillingDerivation {
                outcome: BillingOutcome::Aborted,
                settlement_method: SettlementMethod::Estimated,
                unknown_error_code: false,
            },

            // Pre-provider errors — reserve released, charge = 0.
            Some("context_length_exceeded" | "validation_error") => BillingDerivation {
                outcome: BillingOutcome::Failed,
                settlement_method: SettlementMethod::Released,
                unknown_error_code: false,
            },

            // Post-provider errors — depends on usage availability.
            Some(
                "provider_error"
                | "provider_timeout"
                | "rate_limited"
                | "web_search_calls_exceeded",
            ) => {
                if input.has_usage {
                    BillingDerivation {
                        outcome: BillingOutcome::Failed,
                        settlement_method: SettlementMethod::Actual,
                        unknown_error_code: false,
                    }
                } else {
                    BillingDerivation {
                        outcome: BillingOutcome::Failed,
                        settlement_method: SettlementMethod::Estimated,
                        unknown_error_code: false,
                    }
                }
            }

            // Unknown error code — conservative: Failed/Estimated.
            // Signal caller to log + metric via unknown_error_code flag.
            _ => BillingDerivation {
                outcome: BillingOutcome::Failed,
                settlement_method: SettlementMethod::Estimated,
                unknown_error_code: true,
            },
        },

        // Running should never reach finalization — defensive.
        TurnState::Running => unreachable!("finalization called with Running state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        state: TurnState,
        error_code: Option<&str>,
        has_usage: bool,
    ) -> BillingDerivationInput {
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
}

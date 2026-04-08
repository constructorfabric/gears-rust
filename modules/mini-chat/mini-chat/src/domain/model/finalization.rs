use mini_chat_sdk::RequesterType;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::llm::Usage;
use crate::domain::model::billing_outcome::BillingDerivation;
use crate::domain::model::quota::{SettlementMethod, SettlementOutcome, SettlementPath};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::db::entity::quota_usage::PeriodType;

/// All fields needed by `finalize_turn_cas()`.
///
/// Assembled by the spawned task from `FinalizationCtx` (preflight fields)
/// and `StreamOutcome` (stream result).
#[domain_model]
#[derive(Debug, Clone)]
pub struct FinalizationInput {
    // ── Identity ──
    pub turn_id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub requester_type: RequesterType,
    pub scope: AccessScope,
    pub message_id: Uuid,

    // ── Terminal state (from StreamOutcome) ──
    pub terminal_state: TurnState,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub accumulated_text: String,
    /// Provider-reported usage; `None` if not available.
    pub usage: Option<Usage>,
    pub provider_response_id: Option<String>,

    // ── Quota fields (from preflight, carried via FinalizationCtx) ──
    pub effective_model: String,
    pub selected_model: String,
    pub reserve_tokens: i64,
    pub max_output_tokens_applied: i32,
    pub reserved_credits_micro: i64,
    pub policy_version_applied: i64,
    pub minimal_generation_floor_applied: i32,
    pub quota_decision: String,
    pub downgrade_from: Option<String>,
    pub downgrade_reason: Option<String>,
    pub period_starts: Vec<(PeriodType, time::Date)>,

    // ── Web search telemetry ──
    /// Number of completed web search calls during this turn.
    pub web_search_calls: u32,

    /// Time-to-first-token in milliseconds (captured in `stream_service`).
    pub ttft_ms: Option<u64>,
    /// Total stream duration in milliseconds (captured in `stream_service`).
    pub total_ms: Option<u64>,
}

/// Result of `finalize_turn_cas()`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct FinalizationOutcome {
    pub won_cas: bool,
    pub billing_outcome: Option<BillingDerivation>,
    pub settlement_outcome: Option<SettlementOutcome>,
}

/// Determine whether provider-reported usage is "known" for billing purposes.
///
/// "Usage known" = at least one non-zero token field. A zero-valued or
/// missing usage object is treated as unknown (follows estimated path).
#[must_use]
pub fn has_known_usage(usage: Option<Usage>) -> bool {
    usage.is_some_and(|u| u.input_tokens > 0 || u.output_tokens > 0)
}

/// Build `SettlementPath` from the billing derivation and provider usage.
#[must_use]
pub fn settlement_path_from_billing(
    method: SettlementMethod,
    usage: Option<Usage>,
) -> SettlementPath {
    match method {
        SettlementMethod::Actual => {
            // SAFETY: billing derivation guarantees `usage.is_some()` when method is Actual.
            let u = usage.unwrap_or_else(|| unreachable!("Actual settlement requires usage"));
            SettlementPath::Actual {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            }
        }
        SettlementMethod::Estimated => SettlementPath::Estimated,
        SettlementMethod::Released => SettlementPath::Released,
    }
}

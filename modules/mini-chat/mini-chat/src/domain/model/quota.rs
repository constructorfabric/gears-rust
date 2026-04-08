use modkit_macros::domain_model;
use uuid::Uuid;

use crate::config::EstimationBudgets;
use crate::infra::db::entity::quota_usage::PeriodType;
use mini_chat_sdk::ModelToolSupport;

/// Result of preflight reserve evaluation.
#[domain_model]
#[derive(Debug, Clone)]
pub enum PreflightDecision {
    Allow {
        effective_model: String,
        /// Provider-facing model ID of the effective model (e.g. `"gpt-5.2"`).
        effective_provider_model_id: String,
        reserve_tokens: i64,
        max_output_tokens_applied: i32,
        reserved_credits_micro: i64,
        policy_version_applied: i64,
        minimal_generation_floor_applied: i32,
        /// System prompt for the effective model (from `ModelCatalogEntry`).
        system_prompt: String,
        /// Context window size of the effective model (tokens).
        context_window: u32,
        /// Maximum input tokens of the effective model.
        max_input_tokens: u32,
        /// Per-model estimation budgets from the effective model's catalog entry.
        estimation_budgets: EstimationBudgets,
        /// Top-k chunks for `file_search` (from `ModelCatalogEntry`).
        max_retrieved_chunks_per_turn: u32,
        /// Max tool calls per request (from `ModelCatalogEntry`).
        max_tool_calls: u32,
        /// Tool support flags of the effective model.
        tool_support: ModelToolSupport,
    },
    Downgrade {
        effective_model: String,
        /// Provider-facing model ID of the effective model (e.g. `"gpt-5-mini"`).
        effective_provider_model_id: String,
        reserve_tokens: i64,
        max_output_tokens_applied: i32,
        reserved_credits_micro: i64,
        policy_version_applied: i64,
        minimal_generation_floor_applied: i32,
        downgrade_from: String,
        downgrade_reason: DowngradeReason,
        /// System prompt for the effective model (from `ModelCatalogEntry`).
        system_prompt: String,
        /// Context window size of the effective model (tokens).
        context_window: u32,
        /// Maximum input tokens of the effective model.
        max_input_tokens: u32,
        /// Per-model estimation budgets from the effective model's catalog entry.
        estimation_budgets: EstimationBudgets,
        /// Top-k chunks for `file_search` (from `ModelCatalogEntry`).
        max_retrieved_chunks_per_turn: u32,
        /// Max tool calls per request (from `ModelCatalogEntry`).
        max_tool_calls: u32,
        /// Tool support flags of the effective model.
        tool_support: ModelToolSupport,
    },
    Reject {
        error_code: String,
        http_status: u16,
        quota_scope: String,
    },
}

/// Reason a turn was downgraded from the selected model/tier.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DowngradeReason {
    PremiumQuotaExhausted,
    ForceStandardTier,
    DisablePremiumTier,
    ModelDisabled,
}

impl DowngradeReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PremiumQuotaExhausted => "premium_quota_exhausted",
            Self::ForceStandardTier => "force_standard_tier",
            Self::DisablePremiumTier => "disable_premium_tier",
            Self::ModelDisabled => "model_disabled",
        }
    }
}

/// Result of quota settlement.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SettlementOutcome {
    pub settlement_method: SettlementMethod,
    pub actual_credits_micro: i64,
    pub charged_tokens: u64,
    pub overshoot_capped: bool,
}

/// Which settlement path was used.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementMethod {
    Actual,
    Estimated,
    Released,
}

/// Input to `preflight_reserve()`.
#[domain_model]
#[allow(clippy::struct_excessive_bools)]
pub struct PreflightInput {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub selected_model: String,
    pub utf8_bytes: u64,
    pub num_images: u32,
    pub tools_enabled: bool,
    pub web_search_enabled: bool,
    pub code_interpreter_enabled: bool,
    pub max_output_tokens_cap: u32,
}

/// Input to `settle()`.
#[domain_model]
pub struct SettlementInput {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub effective_model: String,
    pub policy_version_applied: i64,
    pub reserve_tokens: i64,
    pub max_output_tokens_applied: i32,
    pub reserved_credits_micro: i64,
    pub minimal_generation_floor_applied: i32,
    pub settlement_path: SettlementPath,
    pub period_starts: Vec<(PeriodType, time::Date)>,
    /// Completed web search calls to settle.
    pub web_search_calls: u32,
}

/// Classification of the settlement path to take.
#[domain_model]
pub enum SettlementPath {
    /// Provider reported actual usage.
    Actual {
        input_tokens: i64,
        output_tokens: i64,
    },
    /// Provider did not report usage (aborted/failed post-provider-start).
    Estimated,
    /// Pre-provider failure — reserve fully released.
    Released,
}

// ════════════════════════════════════════════════════════════════════════════
// Quota status types — returned by QuotaService for REST endpoint
// ════════════════════════════════════════════════════════════════════════════

/// Full quota status for a user, returned by `QuotaService::get_quota_status()`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct QuotaStatusResult {
    pub tiers: Vec<TierResult>,
    pub warning_threshold_pct: u8,
}

/// Per-tier quota breakdown.
#[domain_model]
#[derive(Debug, Clone)]
pub struct TierResult {
    pub tier: crate::domain::stream_events::QuotaTier,
    pub periods: Vec<PeriodResult>,
}

/// Per-period quota details within a tier.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PeriodResult {
    pub period: crate::domain::stream_events::QuotaPeriod,
    pub limit_credits_micro: i64,
    pub used_credits_micro: i64,
    pub remaining_credits_micro: i64,
    pub remaining_percentage: u8,
    pub next_reset: time::OffsetDateTime,
    pub warning: bool,
    pub exhausted: bool,
}

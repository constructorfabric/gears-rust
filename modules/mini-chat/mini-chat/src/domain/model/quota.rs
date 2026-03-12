use modkit_macros::domain_model;
use uuid::Uuid;

use crate::infra::db::entity::quota_usage::PeriodType;

/// Result of preflight reserve evaluation.
#[domain_model]
#[derive(Debug, Clone)]
pub enum PreflightDecision {
    Allow {
        effective_model: String,
        reserve_tokens: i64,
        max_output_tokens_applied: i32,
        reserved_credits_micro: i64,
        policy_version_applied: i64,
        minimal_generation_floor_applied: i32,
    },
    Downgrade {
        effective_model: String,
        reserve_tokens: i64,
        max_output_tokens_applied: i32,
        reserved_credits_micro: i64,
        policy_version_applied: i64,
        minimal_generation_floor_applied: i32,
        downgrade_from: String,
        downgrade_reason: DowngradeReason,
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
    pub file_search_enabled: bool,
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

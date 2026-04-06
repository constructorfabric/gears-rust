// QuotaService is fully implemented but not yet wired into the turn handler (next phase).
// Remove `dead_code` once preflight_reserve/settle are called from StreamService.
//
// Cast allows: DB columns are BIGINT (i64), domain math uses u64/u32.
// Values are bounded by MAX_TOKENS/MAX_MULT guards in credit_arithmetic.
#![allow(
    dead_code,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::items_after_statements
)]

use std::sync::Arc;

use mini_chat_sdk::{KillSwitches, ModelCatalogEntry, ModelTier, PolicySnapshot, UserLimits};
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::{EstimationBudgets, QuotaConfig};
use crate::domain::error::DomainError;
use crate::domain::model::quota::{
    DowngradeReason, PreflightDecision, PreflightInput, SettlementInput, SettlementMethod,
    SettlementOutcome, SettlementPath,
};
use crate::domain::repos::{PolicySnapshotProvider, QuotaUsageRepository, UserLimitsProvider};
use crate::domain::service::credit_arithmetic::credits_micro_checked;
use crate::domain::service::token_estimator::{self, EstimationInput};
use crate::infra::db::entity::quota_usage::{Model as QuotaUsageModel, PeriodType};

use super::DbProvider;

/// Service handling quota tracking and enforcement.
#[domain_model]
pub struct QuotaService<QR: QuotaUsageRepository> {
    db: Arc<DbProvider>,
    pub(crate) repo: Arc<QR>,
    policy_provider: Arc<dyn PolicySnapshotProvider>,
    limits_provider: Arc<dyn UserLimitsProvider>,
    estimation_budgets: EstimationBudgets,
    quota_config: QuotaConfig,
}

impl<QR: QuotaUsageRepository> QuotaService<QR> {
    pub(crate) fn new(
        db: Arc<DbProvider>,
        repo: Arc<QR>,
        policy_provider: Arc<dyn PolicySnapshotProvider>,
        limits_provider: Arc<dyn UserLimitsProvider>,
        estimation_budgets: EstimationBudgets,
        quota_config: QuotaConfig,
    ) -> Self {
        Self {
            db,
            repo,
            policy_provider,
            limits_provider,
            estimation_budgets,
            quota_config,
        }
    }

    pub(crate) fn web_search_max_calls_per_message(&self) -> u32 {
        self.quota_config.web_search_max_calls_per_message
    }

    pub(crate) fn code_interpreter_max_calls_per_message(&self) -> u32 {
        self.quota_config.code_interpreter_max_calls_per_message
    }

    /// Compute per-tier, per-period quota warnings for the SSE `done` event.
    /// Delegates to `get_quota_status` and flattens the result.
    pub(crate) async fn compute_quota_warnings(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<crate::domain::stream_events::QuotaWarning>, DomainError> {
        use crate::domain::stream_events::QuotaWarning;

        let status = self.get_quota_status(scope, tenant_id, user_id).await?;
        Ok(status
            .tiers
            .into_iter()
            .flat_map(|t| {
                let tier = t.tier;
                t.periods.into_iter().map(move |p| QuotaWarning {
                    tier,
                    period: p.period,
                    remaining_percentage: p.remaining_percentage,
                    warning: p.warning,
                    exhausted: p.exhausted,
                    next_reset: if p.warning || p.exhausted {
                        Some(p.next_reset)
                    } else {
                        None
                    },
                })
            })
            .collect())
    }

    /// Full quota status for the REST endpoint.
    pub(crate) async fn get_quota_status(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<crate::domain::model::quota::QuotaStatusResult, DomainError> {
        use crate::domain::model::quota::{PeriodResult, QuotaStatusResult, TierResult};
        use crate::domain::stream_events::{QuotaPeriod, QuotaTier};

        let conn = self.db.conn()?;

        let version = self.policy_provider.get_current_version(user_id).await?;
        let limits = self.limits_provider.get_limits(user_id, version).await?;

        let today = time::OffsetDateTime::now_utc().date();
        let month_start = today
            .replace_day(1)
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let rows = self
            .repo
            .find_bucket_rows(&conn, scope, tenant_id, user_id)
            .await?;

        let threshold = self.quota_config.warning_threshold_pct;
        let mut tiers = Vec::new();

        for (bucket, tier_enum, tier_limits) in [
            ("tier:premium", QuotaTier::Premium, &limits.premium),
            ("total", QuotaTier::Total, &limits.standard),
        ] {
            let mut periods = Vec::new();
            for (period_type, period_start, limit, period_enum, next_reset) in [
                (
                    PeriodType::Daily,
                    today,
                    tier_limits.limit_daily_credits_micro,
                    QuotaPeriod::Daily,
                    next_daily_reset(today),
                ),
                (
                    PeriodType::Monthly,
                    month_start,
                    tier_limits.limit_monthly_credits_micro,
                    QuotaPeriod::Monthly,
                    next_monthly_reset(today)?,
                ),
            ] {
                if limit <= 0 {
                    continue;
                }

                let used = rows
                    .iter()
                    .find(|r| {
                        r.bucket == bucket
                            && r.period_type == period_type
                            && r.period_start == period_start
                    })
                    .map_or(0, |r| r.spent_credits_micro + r.reserved_credits_micro);

                let remaining = (limit - used).max(0);
                let remaining_pct = remaining_percentage(remaining, limit);

                periods.push(PeriodResult {
                    period: period_enum,
                    limit_credits_micro: limit,
                    used_credits_micro: used,
                    remaining_credits_micro: remaining,
                    remaining_percentage: remaining_pct,
                    next_reset,
                    warning: remaining_pct <= (100 - threshold),
                    exhausted: remaining_pct == 0,
                });
            }
            tiers.push(TierResult {
                tier: tier_enum,
                periods,
            });
        }

        Ok(QuotaStatusResult {
            tiers,
            warning_threshold_pct: threshold,
        })
    }
}

/// Integer percentage of `remaining` relative to `limit` (0..=100).
///
/// Precondition: `limit > 0` and `remaining ∈ [0, limit]`.
#[allow(clippy::integer_division)] // intentional: integer percentage
fn remaining_percentage(remaining: i64, limit: i64) -> u8 {
    debug_assert!(limit > 0, "limit must be positive");
    debug_assert!(remaining >= 0 && remaining <= limit);
    // u128 avoids overflow for large credit values.
    u8::try_from(remaining as u128 * 100 / limit as u128).unwrap_or(100)
}

fn next_daily_reset(today: time::Date) -> time::OffsetDateTime {
    let tomorrow = today + time::Duration::days(1);
    tomorrow.midnight().assume_utc()
}

fn next_monthly_reset(today: time::Date) -> Result<time::OffsetDateTime, DomainError> {
    let next_month = if today.month() == time::Month::December {
        time::Date::from_calendar_date(today.year() + 1, time::Month::January, 1)
    } else {
        time::Date::from_calendar_date(today.year(), today.month().next(), 1)
    };
    Ok(next_month
        .map_err(|e| DomainError::internal(e.to_string()))?
        .midnight()
        .assume_utc())
}

// ── Cascade types ──

#[domain_model]
struct CascadeContext<'a> {
    snapshot: &'a PolicySnapshot,
    user_limits: &'a UserLimits,
    usage_rows: &'a [QuotaUsageModel],
    reserve_credits_micro: i64,
    periods: &'a [(PeriodType, time::Date)],
}

#[domain_model]
enum CascadeDecision {
    Allow {
        effective_model: String,
        tier: ModelTier,
    },
    Downgrade {
        effective_model: String,
        tier: ModelTier,
        downgrade_from: String,
        reason: DowngradeReason,
    },
    Reject,
}

// ── resolve_effective_model ──

impl<QR: QuotaUsageRepository> QuotaService<QR> {
    /// Two-tier downgrade cascade per DESIGN.md §2.2.
    fn resolve_effective_model(selected_model: &str, ctx: &CascadeContext<'_>) -> CascadeDecision {
        let catalog = &ctx.snapshot.model_catalog;
        let ks = &ctx.snapshot.kill_switches;

        // 1. Look up selected model
        let selected_entry = catalog.iter().find(|m| m.model_id == selected_model);

        let (selected_tier, mut downgrade_reason) = match selected_entry {
            Some(e) if e.enabled => (e.tier, None),
            Some(e) => {
                // Model exists but is disabled — cascade from its own tier
                (e.tier, Some(DowngradeReason::ModelDisabled))
            }
            None => {
                // Model not found — start cascade from Premium
                (ModelTier::Premium, Some(DowngradeReason::ModelDisabled))
            }
        };

        // 2. Build cascade from selected tier downward
        let cascade: &[ModelTier] = match selected_tier {
            ModelTier::Premium => &[ModelTier::Premium, ModelTier::Standard],
            ModelTier::Standard => &[ModelTier::Standard],
        };

        // 3. Iterate tiers
        for &tier in cascade {
            // 3a. Kill switch check
            if tier == ModelTier::Premium {
                if ks.force_standard_tier {
                    if downgrade_reason.is_none() {
                        downgrade_reason = Some(DowngradeReason::ForceStandardTier);
                    }
                    continue;
                }
                if ks.disable_premium_tier {
                    if downgrade_reason.is_none() {
                        downgrade_reason = Some(DowngradeReason::DisablePremiumTier);
                    }
                    continue;
                }
            }

            // 3b. Required buckets for this tier
            let buckets: &[&str] = match tier {
                ModelTier::Premium => &["total", "tier:premium"],
                ModelTier::Standard => &["total"],
            };

            // 3c. Check tier availability
            let tier_available = buckets.iter().all(|bucket| {
                ctx.periods.iter().all(|(period_type, period_start)| {
                    let limit = limit_credits_micro(bucket, period_type, ctx.user_limits);
                    let (spent, reserved) =
                        sum_from_usage_rows(bucket, period_type, *period_start, ctx.usage_rows);
                    spent + reserved + ctx.reserve_credits_micro <= limit
                })
            });

            if !tier_available {
                if tier == ModelTier::Premium && downgrade_reason.is_none() {
                    downgrade_reason = Some(DowngradeReason::PremiumQuotaExhausted);
                }
                continue;
            }

            // 3d. Select concrete model for this tier
            //     Prefer the explicitly requested model when it belongs to this
            //     tier and is enabled; fall back to the tier default or any
            //     enabled model otherwise.
            let tier_matches = |m: &&ModelCatalogEntry| m.tier == tier && m.enabled;
            let model = catalog
                .iter()
                .find(|m| tier_matches(m) && m.model_id == selected_model)
                .or_else(|| {
                    catalog
                        .iter()
                        .filter(|m| tier_matches(m))
                        .find(|m| m.preference.as_ref().is_some_and(|p| p.is_default))
                })
                .or_else(|| catalog.iter().find(|m| tier_matches(m)));

            let Some(effective) = model else {
                continue; // all models in tier are individually disabled
            };

            // 3e. Decision
            if effective.model_id == selected_model && downgrade_reason.is_none() {
                return CascadeDecision::Allow {
                    effective_model: effective.model_id.clone(),
                    tier,
                };
            }
            return CascadeDecision::Downgrade {
                effective_model: effective.model_id.clone(),
                tier,
                downgrade_from: selected_model.to_owned(),
                reason: downgrade_reason.unwrap_or(DowngradeReason::PremiumQuotaExhausted),
            };
        }

        // 4. All tiers exhausted
        CascadeDecision::Reject
    }
}

/// Map bucket name + `period_type` to the correct limit from `UserLimits`.
fn limit_credits_micro(bucket: &str, period_type: &PeriodType, limits: &UserLimits) -> i64 {
    let tier = match bucket {
        "total" => &limits.standard,
        "tier:premium" => &limits.premium,
        _ => return 0, // unknown bucket — no budget
    };

    match period_type {
        PeriodType::Daily => tier.limit_daily_credits_micro,
        PeriodType::Monthly => tier.limit_monthly_credits_micro,
    }
}

/// Sum spent + reserved from usage rows for a specific bucket/period.
fn sum_from_usage_rows(
    bucket: &str,
    period_type: &PeriodType,
    period_start: time::Date,
    rows: &[QuotaUsageModel],
) -> (i64, i64) {
    rows.iter()
        .filter(|r| {
            r.bucket == bucket && r.period_type == *period_type && r.period_start == period_start
        })
        .fold((0i64, 0i64), |(spent, reserved), r| {
            (
                spent + r.spent_credits_micro,
                reserved + r.reserved_credits_micro,
            )
        })
}

// ── helpers ──

fn to_db(e: DomainError) -> modkit_db::DbError {
    modkit_db::DbError::Other(anyhow::anyhow!(e))
}

// ── PreflightComputed ──

/// Intermediate result from `preflight_evaluate()`.
/// Contains the decision and all data needed for `preflight_write_reserve()`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PreflightComputed {
    pub decision: PreflightDecision,
    pub(crate) buckets: Vec<String>,
    /// Tier label for metrics, set at construction so reject paths (which have
    /// empty `buckets`) still report the correct tier.
    pub(crate) metrics_tier: &'static str,
    pub(crate) reserved_credits_micro: i64,
    pub(crate) periods: Vec<(PeriodType, time::Date)>,
    pub(crate) tenant_id: uuid::Uuid,
    pub(crate) user_id: uuid::Uuid,
    /// Policy kill switches captured at preflight time.
    /// Avoids a redundant `PolicySnapshotProvider::get()` call in `run_stream()`.
    pub kill_switches: KillSwitches,
}

impl PreflightComputed {
    /// Tier label for metrics recording.
    pub(crate) fn effective_tier(&self) -> &'static str {
        self.metrics_tier
    }
}

// ── preflight_evaluate + preflight_write_reserve ──

impl<QR: QuotaUsageRepository + 'static> QuotaService<QR> {
    /// Evaluate preflight: external I/O, token estimation, cascade decision.
    /// Does NOT write reserves — call `preflight_write_reserve` in the caller's transaction.
    #[allow(clippy::too_many_lines)]
    pub async fn preflight_evaluate(
        &self,
        input: PreflightInput,
    ) -> Result<PreflightComputed, DomainError> {
        // 1. Resolve policy (external I/O)
        let policy_version = self
            .policy_provider
            .get_current_version(input.user_id)
            .await?;
        let snapshot = self
            .policy_provider
            .get_snapshot(input.user_id, policy_version)
            .await?;
        let user_limits = self
            .limits_provider
            .get_limits(input.user_id, policy_version)
            .await?;

        // 1b. Web search kill switch check (before any estimation or DB reads)
        if input.web_search_enabled && snapshot.kill_switches.disable_web_search {
            return Err(DomainError::WebSearchDisabled);
        }

        // 2. Estimate tokens
        let estimation = token_estimator::estimate_tokens(
            &EstimationInput {
                utf8_bytes: input.utf8_bytes,
                num_images: input.num_images,
                tools_enabled: input.tools_enabled,
                web_search_enabled: input.web_search_enabled,
                code_interpreter_enabled: input.code_interpreter_enabled,
            },
            &self.estimation_budgets,
        );

        // 3. Find selected model's multipliers for conservative initial reserve
        let catalog_entry = snapshot
            .model_catalog
            .iter()
            .find(|m| m.model_id == input.selected_model && m.enabled);

        let (in_mult, out_mult) = catalog_entry.map_or(
            (1_000_000, 1_000_000), // fallback for disabled models
            |e| {
                (
                    e.input_tokens_credit_multiplier_micro,
                    e.output_tokens_credit_multiplier_micro,
                )
            },
        );

        // Tier label for metrics — derived from the selected model's catalog
        // entry so that reject paths (which have empty buckets) still report
        // the correct tier.
        let selected_metrics_tier: &'static str = match catalog_entry.map(|e| e.tier) {
            Some(ModelTier::Premium) => "premium",
            _ => "standard",
        };

        // 4. Conservative initial reserve using config cap (pre-cascade)
        let initial_reserved = credits_micro_checked(
            estimation.estimated_input_tokens,
            u64::from(input.max_output_tokens_cap),
            in_mult,
            out_mult,
        )
        .map_err(|e| DomainError::internal(e.to_string()))?;

        // 5. Compute period boundaries
        let now = OffsetDateTime::now_utc().date();
        let month_start = now
            .replace_day(1)
            .map_err(|e| DomainError::internal(e.to_string()))?;
        let periods = vec![(PeriodType::Daily, now), (PeriodType::Monthly, month_start)];

        // 6. Transaction: lock rows, check web search quota, run cascade
        let repo = Arc::clone(&self.repo);
        let tenant_id = input.tenant_id;
        let user_id = input.user_id;
        let selected_model = input.selected_model.clone();
        let max_output_tokens_cap = input.max_output_tokens_cap;
        let estimation_budgets = self.estimation_budgets;
        let web_search_daily_quota = self.quota_config.web_search_daily_quota;
        let code_interpreter_daily_quota = self.quota_config.code_interpreter_daily_quota;

        let tx_result = self
            .db
            .transaction(|tx| {
                let snapshot = snapshot.clone();
                let user_limits = user_limits.clone();
                let periods = periods.clone();
                let selected_model = selected_model.clone();
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(tenant_id);

                    let period_types: Vec<PeriodType> =
                        periods.iter().map(|(pt, _)| pt.clone()).collect();
                    let period_starts: Vec<time::Date> =
                        periods.iter().map(|(_, ps)| *ps).collect();

                    let usage_rows = repo
                        .find_bucket_rows_for_update(
                            tx,
                            &scope,
                            tenant_id,
                            user_id,
                            &period_types,
                            &period_starts,
                        )
                        .await
                        .map_err(to_db)?;

                    let cascade_ctx = CascadeContext {
                        snapshot: &snapshot,
                        user_limits: &user_limits,
                        usage_rows: &usage_rows,
                        reserve_credits_micro: initial_reserved,
                        periods: &periods,
                    };

                    let decision = Self::resolve_effective_model(&selected_model, &cascade_ctx);

                    match decision {
                        CascadeDecision::Reject => Ok(PreflightComputed {
                            decision: PreflightDecision::Reject {
                                error_code: "quota_exceeded".to_owned(),
                                http_status: 429,
                                quota_scope: "tokens".to_owned(),
                            },
                            buckets: vec![],
                            // Cascade exhausted to standard before rejecting.
                            metrics_tier: "standard",
                            reserved_credits_micro: 0,
                            periods: periods.clone(),
                            tenant_id,
                            user_id,
                            kill_switches: snapshot.kill_switches.clone(),
                        }),
                        CascadeDecision::Allow {
                            ref effective_model,
                            tier,
                        }
                        | CascadeDecision::Downgrade {
                            ref effective_model,
                            tier,
                            ..
                        } => {
                            let effective_model = effective_model.clone();

                            // Look up effective model's catalog entry for per-model max_output
                            let eff_entry = snapshot
                                .model_catalog
                                .iter()
                                .find(|m| m.model_id == effective_model)
                                .ok_or_else(|| {
                                    to_db(DomainError::internal("effective model not in catalog"))
                                })?;

                            // 6a. Daily web search quota check (post-cascade)
                            if eff_entry.general_config.tool_support.web_search {
                                let today = period_starts[0];
                                let daily_web_search_calls = repo
                                    .get_daily_web_search_calls(
                                        tx, &scope, tenant_id, user_id, today,
                                    )
                                    .await
                                    .map_err(to_db)?;
                                if daily_web_search_calls >= web_search_daily_quota {
                                    return Ok(PreflightComputed {
                                        decision: PreflightDecision::Reject {
                                            error_code: "quota_exceeded".to_owned(),
                                            http_status: 429,
                                            quota_scope: "web_search".to_owned(),
                                        },
                                        buckets: vec![],
                                        metrics_tier: selected_metrics_tier,
                                        reserved_credits_micro: 0,
                                        periods: periods.clone(),
                                        tenant_id,
                                        user_id,
                                        kill_switches: snapshot.kill_switches.clone(),
                                    });
                                }
                            }

                            // 6b. Daily code interpreter quota check (post-cascade)
                            if eff_entry.general_config.tool_support.code_interpreter {
                                let today = period_starts[0];
                                let daily_ci_calls = repo
                                    .get_daily_code_interpreter_calls(
                                        tx, &scope, tenant_id, user_id, today,
                                    )
                                    .await
                                    .map_err(to_db)?;
                                if daily_ci_calls >= code_interpreter_daily_quota {
                                    return Ok(PreflightComputed {
                                        decision: PreflightDecision::Reject {
                                            error_code: "quota_exceeded".to_owned(),
                                            http_status: 429,
                                            quota_scope: "code_interpreter".to_owned(),
                                        },
                                        buckets: vec![],
                                        metrics_tier: selected_metrics_tier,
                                        reserved_credits_micro: 0,
                                        periods: periods.clone(),
                                        tenant_id,
                                        user_id,
                                        kill_switches: snapshot.kill_switches.clone(),
                                    });
                                }
                            }

                            // Resolve per-model max_output_tokens: min(catalog, config cap)
                            let max_output_tokens_applied =
                                std::cmp::min(eff_entry.max_output_tokens, max_output_tokens_cap);

                            // Recompute credits with effective model's multipliers and resolved max_output
                            let final_reserved = credits_micro_checked(
                                estimation.estimated_input_tokens,
                                max_output_tokens_applied as u64,
                                eff_entry.input_tokens_credit_multiplier_micro,
                                eff_entry.output_tokens_credit_multiplier_micro,
                            )
                            .map_err(|e| to_db(DomainError::internal(e.to_string())))?;

                            let buckets: Vec<String> = match tier {
                                ModelTier::Premium => {
                                    vec!["total".to_owned(), "tier:premium".to_owned()]
                                }
                                ModelTier::Standard => vec!["total".to_owned()],
                            };

                            let reserve_tokens = estimation
                                .estimated_input_tokens
                                .saturating_add(max_output_tokens_applied as u64)
                                as i64;
                            let max_output_tokens_applied = max_output_tokens_applied as i32;
                            let policy_version_applied = policy_version as i64;
                            let minimal_generation_floor_applied =
                                estimation_budgets.minimal_generation_floor as i32;

                            let system_prompt = eff_entry.system_prompt.clone();

                            let model_estimation_budgets = EstimationBudgets {
                                bytes_per_token_conservative: eff_entry
                                    .estimation_budgets
                                    .bytes_per_token_conservative,
                                fixed_overhead_tokens: eff_entry
                                    .estimation_budgets
                                    .fixed_overhead_tokens,
                                safety_margin_pct: eff_entry.estimation_budgets.safety_margin_pct,
                                image_token_budget: eff_entry.estimation_budgets.image_token_budget,
                                tool_surcharge_tokens: eff_entry
                                    .estimation_budgets
                                    .tool_surcharge_tokens,
                                web_search_surcharge_tokens: eff_entry
                                    .estimation_budgets
                                    .web_search_surcharge_tokens,
                                code_interpreter_surcharge_tokens: eff_entry
                                    .estimation_budgets
                                    .code_interpreter_surcharge_tokens,
                                minimal_generation_floor: eff_entry
                                    .estimation_budgets
                                    .minimal_generation_floor,
                            };

                            let preflight_decision = match decision {
                                CascadeDecision::Allow { .. } => PreflightDecision::Allow {
                                    effective_model,
                                    effective_provider_model_id: eff_entry
                                        .provider_model_id
                                        .clone(),
                                    reserve_tokens,
                                    max_output_tokens_applied,
                                    reserved_credits_micro: final_reserved,
                                    policy_version_applied,
                                    minimal_generation_floor_applied,
                                    system_prompt,
                                    context_window: eff_entry.context_window,
                                    max_input_tokens: eff_entry.max_input_tokens,
                                    estimation_budgets: model_estimation_budgets,
                                    max_retrieved_chunks_per_turn: eff_entry
                                        .max_retrieved_chunks_per_turn,
                                    max_tool_calls: eff_entry.max_tool_calls,
                                    tool_support: eff_entry.general_config.tool_support.clone(),
                                    api_params: eff_entry.general_config.api_params.clone(),
                                },
                                CascadeDecision::Downgrade {
                                    downgrade_from,
                                    reason,
                                    ..
                                } => PreflightDecision::Downgrade {
                                    effective_model,
                                    effective_provider_model_id: eff_entry
                                        .provider_model_id
                                        .clone(),
                                    reserve_tokens,
                                    max_output_tokens_applied,
                                    reserved_credits_micro: final_reserved,
                                    policy_version_applied,
                                    minimal_generation_floor_applied,
                                    downgrade_from,
                                    downgrade_reason: reason,
                                    system_prompt,
                                    context_window: eff_entry.context_window,
                                    max_input_tokens: eff_entry.max_input_tokens,
                                    estimation_budgets: model_estimation_budgets,
                                    max_retrieved_chunks_per_turn: eff_entry
                                        .max_retrieved_chunks_per_turn,
                                    max_tool_calls: eff_entry.max_tool_calls,
                                    tool_support: eff_entry.general_config.tool_support.clone(),
                                    api_params: eff_entry.general_config.api_params.clone(),
                                },
                                CascadeDecision::Reject => unreachable!(),
                            };

                            let metrics_tier = match tier {
                                ModelTier::Premium => "premium",
                                ModelTier::Standard => "standard",
                            };

                            Ok(PreflightComputed {
                                decision: preflight_decision,
                                buckets,
                                metrics_tier,
                                reserved_credits_micro: final_reserved,
                                periods: periods.clone(),
                                tenant_id,
                                user_id,
                                kill_switches: snapshot.kill_switches.clone(),
                            })
                        }
                    }
                })
            })
            .await;

        tx_result.map_err(DomainError::from)
    }

    /// Write the reserve increments. Call inside the caller's transaction
    /// alongside turn/message creation for atomicity.
    pub async fn preflight_write_reserve(
        &self,
        runner: &impl DBRunner,
        computed: &PreflightComputed,
    ) -> Result<(), DomainError> {
        // No-op for Reject decisions
        if computed.buckets.is_empty() {
            return Ok(());
        }

        let scope = AccessScope::for_tenant(computed.tenant_id);

        use crate::domain::repos::IncrementReserveParams;
        for bucket in &computed.buckets {
            for (period_type, period_start) in &computed.periods {
                self.repo
                    .increment_reserve(
                        runner,
                        &scope,
                        IncrementReserveParams {
                            tenant_id: computed.tenant_id,
                            user_id: computed.user_id,
                            period_type: period_type.clone(),
                            period_start: *period_start,
                            bucket: bucket.clone(),
                            amount_micro: computed.reserved_credits_micro,
                        },
                    )
                    .await?;
            }
        }

        Ok(())
    }

    /// Combined evaluate + write for backward compatibility.
    /// Delegates to `preflight_evaluate` + `preflight_write_reserve`.
    pub async fn preflight_reserve(
        &self,
        input: PreflightInput,
    ) -> Result<PreflightDecision, DomainError> {
        let computed = self.preflight_evaluate(input).await?;

        // Write reserve in its own transaction (legacy behavior)
        let repo = Arc::clone(&self.repo);
        let buckets = computed.buckets.clone();
        let reserved_credits_micro = computed.reserved_credits_micro;
        let periods = computed.periods.clone();
        let tenant_id = computed.tenant_id;
        let user_id = computed.user_id;

        if !buckets.is_empty() {
            self.db
                .transaction(|tx| {
                    let buckets = buckets.clone();
                    let periods = periods.clone();
                    Box::pin(async move {
                        let scope = AccessScope::for_tenant(tenant_id);

                        use crate::domain::repos::IncrementReserveParams;
                        for bucket in &buckets {
                            for (period_type, period_start) in &periods {
                                repo.increment_reserve(
                                    tx,
                                    &scope,
                                    IncrementReserveParams {
                                        tenant_id,
                                        user_id,
                                        period_type: period_type.clone(),
                                        period_start: *period_start,
                                        bucket: bucket.clone(),
                                        amount_micro: reserved_credits_micro,
                                    },
                                )
                                .await
                                .map_err(to_db)?;
                            }
                        }
                        Ok(())
                    })
                })
                .await
                .map_err(DomainError::from)?;
        }

        Ok(computed.decision)
    }
}

// ── settle ──

impl<QR: QuotaUsageRepository> QuotaService<QR> {
    pub async fn settle(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        input: SettlementInput,
    ) -> Result<SettlementOutcome, DomainError> {
        Self::validate_settlement_input(&input)?;

        // Load snapshot for policy_version_applied (never current)
        let snapshot = self
            .policy_provider
            .get_snapshot(input.user_id, input.policy_version_applied as u64)
            .await?;

        let catalog_entry = snapshot
            .model_catalog
            .iter()
            .find(|m| m.model_id == input.effective_model)
            .ok_or_else(|| {
                DomainError::internal(format!(
                    "model {} not found in policy version {}",
                    input.effective_model, input.policy_version_applied
                ))
            })?;

        let tier = catalog_entry.tier;
        let in_mult = catalog_entry.input_tokens_credit_multiplier_micro;
        let out_mult = catalog_entry.output_tokens_credit_multiplier_micro;

        let buckets: Vec<&str> = match tier {
            ModelTier::Premium => vec!["total", "tier:premium"],
            ModelTier::Standard => vec!["total"],
        };

        let outcome = match input.settlement_path {
            SettlementPath::Actual {
                input_tokens,
                output_tokens,
            } => {
                self.settle_actual(
                    runner,
                    scope,
                    &input,
                    &buckets,
                    in_mult,
                    out_mult,
                    input_tokens,
                    output_tokens,
                )
                .await?
            }
            SettlementPath::Estimated => {
                self.settle_estimated(runner, scope, &input, &buckets, in_mult, out_mult)
                    .await?
            }
            SettlementPath::Released => {
                self.settle_released(runner, scope, &input, &buckets)
                    .await?
            }
        };

        Ok(outcome)
    }

    fn validate_settlement_input(input: &SettlementInput) -> Result<(), DomainError> {
        if input.reserve_tokens <= 0 {
            return Err(DomainError::internal(
                "invalid settlement input: reserve_tokens must be positive",
            ));
        }
        if input.max_output_tokens_applied < 0 {
            return Err(DomainError::internal(
                "invalid settlement input: negative max_output_tokens_applied",
            ));
        }
        if input.minimal_generation_floor_applied < 0 {
            return Err(DomainError::internal(
                "invalid settlement input: negative minimal_generation_floor_applied",
            ));
        }
        if input.reserved_credits_micro < 0 {
            return Err(DomainError::internal(
                "invalid settlement input: negative reserved_credits_micro",
            ));
        }
        if let SettlementPath::Actual {
            input_tokens,
            output_tokens,
        } = &input.settlement_path
            && (*input_tokens < 0 || *output_tokens < 0)
        {
            return Err(DomainError::internal(
                "invalid settlement input: negative actual token counts",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_actual(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        input: &SettlementInput,
        buckets: &[&str],
        in_mult: u64,
        out_mult: u64,
        actual_input: i64,
        actual_output: i64,
    ) -> Result<SettlementOutcome, DomainError> {
        let actual_credits =
            credits_micro_checked(actual_input as u64, actual_output as u64, in_mult, out_mult)
                .map_err(|e| DomainError::internal(e.to_string()))?;

        let actual_tokens = actual_input + actual_output;
        let (committed_credits, overshoot_capped, charged_tokens) =
            if actual_tokens > input.reserve_tokens {
                let overshoot_factor = actual_tokens as f64 / input.reserve_tokens as f64;
                if overshoot_factor > self.quota_config.overshoot_tolerance_factor {
                    (
                        input.reserved_credits_micro,
                        true,
                        input.reserve_tokens as u64,
                    )
                } else {
                    (actual_credits, false, actual_tokens as u64)
                }
            } else {
                (actual_credits, false, actual_tokens as u64)
            };

        use crate::domain::repos::SettleParams;
        for bucket in buckets {
            let is_total = *bucket == "total";
            for (period_type, period_start) in &input.period_starts {
                self.repo
                    .settle(
                        runner,
                        scope,
                        SettleParams {
                            tenant_id: input.tenant_id,
                            user_id: input.user_id,
                            period_type: period_type.clone(),
                            period_start: *period_start,
                            bucket: (*bucket).to_owned(),
                            reserved_credits_micro: input.reserved_credits_micro,
                            actual_credits_micro: committed_credits,
                            input_tokens: if is_total { Some(actual_input) } else { None },
                            output_tokens: if is_total { Some(actual_output) } else { None },
                            web_search_calls: input.web_search_calls,
                            code_interpreter_calls: input.code_interpreter_calls,
                        },
                    )
                    .await?;
            }
        }

        Ok(SettlementOutcome {
            settlement_method: SettlementMethod::Actual,
            actual_credits_micro: committed_credits,
            charged_tokens,
            overshoot_capped,
        })
    }

    async fn settle_estimated(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        input: &SettlementInput,
        buckets: &[&str],
        in_mult: u64,
        out_mult: u64,
    ) -> Result<SettlementOutcome, DomainError> {
        let estimated_input_tokens =
            (input.reserve_tokens - input.max_output_tokens_applied as i64).max(0);
        let charged_output_tokens = input.minimal_generation_floor_applied as i64;
        let charged_tokens = std::cmp::min(
            input.reserve_tokens,
            estimated_input_tokens + charged_output_tokens,
        );

        let actual_credits = credits_micro_checked(
            estimated_input_tokens as u64, // safe: clamped to >= 0 above
            charged_output_tokens as u64,
            in_mult,
            out_mult,
        )
        .map_err(|e| DomainError::internal(e.to_string()))?;

        use crate::domain::repos::SettleParams;
        for bucket in buckets {
            for (period_type, period_start) in &input.period_starts {
                self.repo
                    .settle(
                        runner,
                        scope,
                        SettleParams {
                            tenant_id: input.tenant_id,
                            user_id: input.user_id,
                            period_type: period_type.clone(),
                            period_start: *period_start,
                            bucket: (*bucket).to_owned(),
                            reserved_credits_micro: input.reserved_credits_micro,
                            actual_credits_micro: actual_credits,
                            input_tokens: None,
                            output_tokens: None,
                            web_search_calls: input.web_search_calls,
                            code_interpreter_calls: input.code_interpreter_calls,
                        },
                    )
                    .await?;
            }
        }

        Ok(SettlementOutcome {
            settlement_method: SettlementMethod::Estimated,
            actual_credits_micro: actual_credits,
            charged_tokens: charged_tokens.max(0) as u64,
            overshoot_capped: false,
        })
    }

    async fn settle_released(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        input: &SettlementInput,
        buckets: &[&str],
    ) -> Result<SettlementOutcome, DomainError> {
        use crate::domain::repos::SettleParams;
        for bucket in buckets {
            for (period_type, period_start) in &input.period_starts {
                self.repo
                    .settle(
                        runner,
                        scope,
                        SettleParams {
                            tenant_id: input.tenant_id,
                            user_id: input.user_id,
                            period_type: period_type.clone(),
                            period_start: *period_start,
                            bucket: (*bucket).to_owned(),
                            reserved_credits_micro: input.reserved_credits_micro,
                            actual_credits_micro: 0,
                            input_tokens: None,
                            output_tokens: None,
                            web_search_calls: 0,
                            code_interpreter_calls: 0,
                        },
                    )
                    .await?;
            }
        }

        Ok(SettlementOutcome {
            settlement_method: SettlementMethod::Released,
            actual_credits_micro: 0,
            charged_tokens: 0,
            overshoot_capped: false,
        })
    }
}
#[cfg(test)]
#[path = "quota_service_tests.rs"]
mod tests;

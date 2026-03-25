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
mod tests {
    use super::*;
    use crate::domain::service::test_helpers::{TestCatalogEntryParams, test_catalog_entry};
    use mini_chat_sdk::{KillSwitches, ModelCatalogEntry, TierLimits};
    use uuid::Uuid;

    fn make_model(id: &str, tier: ModelTier, enabled: bool, is_default: bool) -> ModelCatalogEntry {
        test_catalog_entry(TestCatalogEntryParams {
            model_id: id.to_owned(),
            provider_model_id: format!("provider-{id}"),
            display_name: id.to_owned(),
            tier,
            enabled,
            is_default,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 1_000_000,
            multimodal_capabilities: vec![],
            context_window: 128_000,
            max_output_tokens: 4096,
            description: String::new(),
            provider_display_name: String::new(),
            multiplier_display: "1x".to_owned(),
            provider_id: "openai".to_owned(),
        })
    }

    fn default_limits() -> UserLimits {
        UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: TierLimits {
                limit_daily_credits_micro: 50_000_000,
                limit_monthly_credits_micro: 500_000_000,
            },
        }
    }

    fn default_snapshot() -> PolicySnapshot {
        PolicySnapshot {
            user_id: Uuid::nil(),
            policy_version: 1,
            model_catalog: vec![
                make_model("gpt-5", ModelTier::Premium, true, true),
                make_model("gpt-5-mini", ModelTier::Standard, true, true),
            ],
            kill_switches: KillSwitches::default(),
        }
    }

    fn default_periods(today: time::Date) -> Vec<(PeriodType, time::Date)> {
        let month_start = today.replace_day(1).unwrap();
        vec![
            (PeriodType::Daily, today),
            (PeriodType::Monthly, month_start),
        ]
    }

    // ── 8.5: resolve_effective_model tests ──

    #[test]
    fn premium_available_returns_allow() {
        let snapshot = default_snapshot();
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Allow { effective_model, tier } => {
                assert_eq!(effective_model, "gpt-5");
                assert_eq!(tier, ModelTier::Premium);
            }
            other => panic!("expected Allow, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn premium_exhausted_downgrades_to_standard() {
        let snapshot = default_snapshot();
        let mut limits = default_limits();
        // Set premium daily limit very low so it's exhausted
        limits.premium.limit_daily_credits_micro = 0;
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Downgrade { effective_model, tier, reason, .. } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(tier, ModelTier::Standard);
                assert_eq!(reason, DowngradeReason::PremiumQuotaExhausted);
            }
            other => panic!("expected Downgrade, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn standard_selected_skips_premium() {
        let snapshot = default_snapshot();
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5-mini", &ctx) {
            CascadeDecision::Allow { effective_model, tier } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(tier, ModelTier::Standard);
            }
            other => panic!("expected Allow, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn all_exhausted_returns_reject() {
        let snapshot = default_snapshot();
        let mut limits = default_limits();
        limits.premium.limit_daily_credits_micro = 0;
        limits.standard.limit_daily_credits_micro = 0;
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Reject => {}
            other => panic!("expected Reject, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn disable_premium_tier_downgrades() {
        let mut snapshot = default_snapshot();
        snapshot.kill_switches.disable_premium_tier = true;
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Downgrade { effective_model, reason, .. } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(reason, DowngradeReason::DisablePremiumTier);
            }
            other => panic!("expected Downgrade, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn force_standard_tier_downgrades() {
        let mut snapshot = default_snapshot();
        snapshot.kill_switches.force_standard_tier = true;
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Downgrade { effective_model, reason, .. } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(reason, DowngradeReason::ForceStandardTier);
            }
            other => panic!("expected Downgrade, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn disabled_model_triggers_downgrade() {
        let mut snapshot = default_snapshot();
        // Disable the selected premium model
        snapshot.model_catalog[0].enabled = false;
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Downgrade { effective_model, reason, .. } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(reason, DowngradeReason::ModelDisabled);
            }
            other => panic!("expected Downgrade, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn disabled_standard_model_does_not_escalate_to_premium() {
        let mut snapshot = default_snapshot();
        // Disable the standard model (index 1 = gpt-5-mini)
        snapshot.model_catalog[1].enabled = false;
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        // Selecting the disabled standard model should reject, NOT escalate to premium
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5-mini", &ctx) {
            CascadeDecision::Reject => {}
            other => panic!("expected Reject (no standard models available), got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn requested_non_default_model_is_preserved() {
        // Regression: selecting a non-default model in the same tier must NOT
        // silently rewrite to the tier default.
        let snapshot = PolicySnapshot {
            user_id: Uuid::nil(),
            policy_version: 1,
            model_catalog: vec![
                make_model("std-default", ModelTier::Standard, true, true),
                make_model("std-other", ModelTier::Standard, true, false),
            ],
            kill_switches: KillSwitches::default(),
        };
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("std-other", &ctx) {
            CascadeDecision::Allow { effective_model, tier } => {
                assert_eq!(effective_model, "std-other", "must preserve explicitly requested model");
                assert_eq!(tier, ModelTier::Standard);
            }
            other => panic!("expected Allow with std-other, got {:?}", cascade_debug(&other)),
        }
    }

    #[test]
    fn all_models_disabled_returns_reject() {
        let mut snapshot = default_snapshot();
        for m in &mut snapshot.model_catalog {
            m.enabled = false;
        }
        let limits = default_limits();
        let today = OffsetDateTime::now_utc().date();
        let periods = default_periods(today);
        let ctx = CascadeContext {
            snapshot: &snapshot,
            user_limits: &limits,
            usage_rows: &[],
            reserve_credits_micro: 1_000,
            periods: &periods,
        };
        match QuotaService::<crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository>::resolve_effective_model("gpt-5", &ctx) {
            CascadeDecision::Reject => {}
            other => panic!("expected Reject, got {:?}", cascade_debug(&other)),
        }
    }

    fn cascade_debug(d: &CascadeDecision) -> String {
        match d {
            CascadeDecision::Allow {
                effective_model,
                tier,
            } => {
                format!("Allow({effective_model}, {tier:?})")
            }
            CascadeDecision::Downgrade {
                effective_model,
                tier,
                reason,
                ..
            } => {
                format!("Downgrade({effective_model}, {tier:?}, {reason:?})")
            }
            CascadeDecision::Reject => "Reject".to_owned(),
        }
    }

    // ── 9.4–9.7: Settlement tests ──

    use crate::config::QuotaConfig;
    use crate::domain::service::test_helpers::{
        MockPolicySnapshotProvider, MockUserLimitsProvider, inmem_db, mock_db_provider,
    };
    use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository as QuotaUsageRepo;

    type TestQuotaService = QuotaService<QuotaUsageRepo>;

    fn make_test_service(
        db: Arc<DbProvider>,
        snapshot: PolicySnapshot,
        overshoot_tolerance: f64,
    ) -> TestQuotaService {
        TestQuotaService::new(
            db,
            Arc::new(QuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(snapshot)),
            Arc::new(MockUserLimitsProvider::new(default_limits())),
            crate::config::EstimationBudgets::default(),
            QuotaConfig {
                overshoot_tolerance_factor: overshoot_tolerance,
                ..QuotaConfig::default()
            },
        )
    }

    fn settlement_input(
        model: &str,
        _tier: ModelTier,
        reserve_tokens: i64,
        reserved_credits_micro: i64,
        path: SettlementPath,
        today: time::Date,
    ) -> SettlementInput {
        SettlementInput {
            tenant_id: Uuid::nil(),
            user_id: Uuid::nil(),
            effective_model: model.to_owned(),
            policy_version_applied: 1,
            reserve_tokens,
            max_output_tokens_applied: 1000,
            reserved_credits_micro,
            minimal_generation_floor_applied: 50,
            settlement_path: path,
            period_starts: default_periods(today),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        }
    }

    /// Pre-populate `quota_usage` rows so `settle()` can decrement them.
    async fn seed_reserve(
        db: &DbProvider,
        model_tier: ModelTier,
        reserved_credits_micro: i64,
        today: time::Date,
    ) {
        use crate::domain::repos::IncrementReserveParams;
        use crate::domain::repos::QuotaUsageRepository as QURepo;

        let scope = AccessScope::for_tenant(Uuid::nil());
        let conn = db.conn().unwrap();
        let repo = QuotaUsageRepo;

        let buckets: Vec<&str> = match model_tier {
            ModelTier::Premium => vec!["total", "tier:premium"],
            ModelTier::Standard => vec!["total"],
        };

        for bucket in &buckets {
            for (period_type, period_start) in &default_periods(today) {
                repo.increment_reserve(
                    &conn,
                    &scope,
                    IncrementReserveParams {
                        tenant_id: Uuid::nil(),
                        user_id: Uuid::nil(),
                        period_type: period_type.clone(),
                        period_start: *period_start,
                        bucket: (*bucket).to_owned(),
                        amount_micro: reserved_credits_micro,
                    },
                )
                .await
                .unwrap();
            }
        }
    }

    // 9.4: Actual settlement — normal (no overshoot)
    #[tokio::test]
    async fn settle_actual_normal() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            2000,   // reserve_tokens
            10_000, // reserved_credits_micro
            SettlementPath::Actual {
                input_tokens: 800,
                output_tokens: 200,
            },
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);
        assert!(!outcome.overshoot_capped);
        assert_eq!(outcome.charged_tokens, 1000);
        // credits = ceil_div(800 * 1_000_000, 1_000_000) + ceil_div(200 * 1_000_000, 1_000_000)
        // = 800 + 200 = 1000
        assert_eq!(outcome.actual_credits_micro, 1000);
    }

    // 9.4: Actual settlement — within tolerance
    #[tokio::test]
    async fn settle_actual_within_tolerance() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        // actual tokens = 1050, reserve = 1000 → overshoot 1.05 < 1.10 tolerance
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            1000,
            10_000,
            SettlementPath::Actual {
                input_tokens: 800,
                output_tokens: 250,
            },
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);
        assert!(!outcome.overshoot_capped);
        assert_eq!(outcome.actual_credits_micro, 1050);
    }

    // 9.4: Actual settlement — exceeds tolerance (caps at reserve)
    #[tokio::test]
    async fn settle_actual_exceeds_tolerance_caps_at_reserve() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        // actual tokens = 1500, reserve = 1000 → overshoot 1.50 > 1.10 tolerance
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            1000,
            10_000,
            SettlementPath::Actual {
                input_tokens: 1000,
                output_tokens: 500,
            },
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);
        assert!(outcome.overshoot_capped);
        assert_eq!(outcome.actual_credits_micro, 10_000); // capped at reserved
        assert_eq!(outcome.charged_tokens, 1000); // capped at reserve_tokens
    }

    // 9.5: Estimated settlement
    #[tokio::test]
    async fn settle_estimated_deterministic() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            2000,   // reserve_tokens
            10_000, // reserved_credits_micro
            SettlementPath::Estimated,
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Estimated);
        assert!(!outcome.overshoot_capped);
        // estimated_input_tokens = reserve_tokens - max_output_tokens_applied = 2000 - 1000 = 1000
        // charged_output_tokens = minimal_generation_floor_applied = 50
        // charged_tokens = min(2000, 1000 + 50) = 1050
        assert_eq!(outcome.charged_tokens, 1050);
        // credits = ceil_div(1000*1M, 1M) + ceil_div(50*1M, 1M) = 1000 + 50 = 1050
        assert_eq!(outcome.actual_credits_micro, 1050);
    }

    // 9.5: Same inputs → same output
    #[tokio::test]
    async fn settle_estimated_same_inputs_same_output() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        // Seed enough for two settlements
        seed_reserve(&db, ModelTier::Premium, 20_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());

        let make_input = || {
            settlement_input(
                "gpt-5",
                ModelTier::Premium,
                2000,
                10_000,
                SettlementPath::Estimated,
                today,
            )
        };

        let outcome1 = svc.settle(&conn, &scope, make_input()).await.unwrap();
        let outcome2 = svc.settle(&conn, &scope, make_input()).await.unwrap();
        assert_eq!(outcome1.actual_credits_micro, outcome2.actual_credits_micro);
        assert_eq!(outcome1.charged_tokens, outcome2.charged_tokens);
    }

    // 9.5: Estimated never exceeds reserve
    #[tokio::test]
    async fn settle_estimated_never_exceeds_reserve() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 500, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        // reserve_tokens = 100, max_output = 1000 → estimated_input = -900 → cast to u64 would overflow
        // Actually we need sane values. Let's set max_output > reserve_tokens.
        let mut input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            100,
            500,
            SettlementPath::Estimated,
            today,
        );
        input.max_output_tokens_applied = 200;
        input.minimal_generation_floor_applied = 50;
        // estimated_input = max(100 - 200, 0) = 0 (clamped, no wraparound)
        // charged_output = 50
        // charged_tokens = min(100, 0 + 50) = 50
        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.charged_tokens, 50);
        assert!(outcome.charged_tokens <= 100);
    }

    // 9.6: Released settlement
    #[tokio::test]
    async fn settle_released_zero_credits() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            2000,
            10_000,
            SettlementPath::Released,
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Released);
        assert_eq!(outcome.actual_credits_micro, 0);
        assert_eq!(outcome.charged_tokens, 0);
        assert!(!outcome.overshoot_capped);
    }

    // 9.7: Premium turn updates total + tier:premium
    #[tokio::test]
    async fn settle_premium_updates_both_buckets() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Premium, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let input = settlement_input(
            "gpt-5",
            ModelTier::Premium,
            2000,
            10_000,
            SettlementPath::Actual {
                input_tokens: 500,
                output_tokens: 500,
            },
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);

        // Verify both buckets were updated by reading rows
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        let total_rows: Vec<_> = rows.iter().filter(|r| r.bucket == "total").collect();
        let premium_rows: Vec<_> = rows.iter().filter(|r| r.bucket == "tier:premium").collect();
        assert!(!total_rows.is_empty(), "total bucket should have rows");
        assert!(
            !premium_rows.is_empty(),
            "tier:premium bucket should have rows"
        );

        // Both should have spent > 0 and reserve decremented
        for row in &total_rows {
            assert!(row.spent_credits_micro > 0);
        }
        for row in &premium_rows {
            assert!(row.spent_credits_micro > 0);
        }
    }

    // ── 8.6: preflight_reserve tests ──

    fn preflight_input(selected_model: &str) -> PreflightInput {
        PreflightInput {
            tenant_id: Uuid::nil(),
            user_id: Uuid::nil(),
            selected_model: selected_model.to_owned(),
            utf8_bytes: 4000,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
            max_output_tokens_cap: 4096,
        }
    }

    #[tokio::test]
    async fn preflight_allow_returns_all_fields() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        match result {
            PreflightDecision::Allow {
                effective_model,
                reserve_tokens,
                max_output_tokens_applied,
                reserved_credits_micro,
                policy_version_applied,
                minimal_generation_floor_applied,
                ..
            } => {
                assert_eq!(effective_model, "gpt-5");
                assert!(reserve_tokens > 0);
                assert_eq!(max_output_tokens_applied, 4096);
                assert!(reserved_credits_micro > 0);
                assert_eq!(policy_version_applied, 1);
                assert!(minimal_generation_floor_applied > 0);
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_max_output_tokens_capped_by_model_catalog() {
        // Task 3.7: model max_output_tokens=4096, config cap=32768 → applied=4096
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot(); // catalog has max_output_tokens: 4096
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let mut input = preflight_input("gpt-5");
        input.max_output_tokens_cap = 32768; // config cap much larger than model

        let result = svc.preflight_reserve(input).await.unwrap();
        match result {
            PreflightDecision::Allow {
                max_output_tokens_applied,
                ..
            } => {
                assert_eq!(max_output_tokens_applied, 4096); // model's value wins
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_max_output_tokens_capped_by_config() {
        // Task 3.8: model max_output_tokens=65536, config cap=32768 → applied=32768
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        for entry in &mut snapshot.model_catalog {
            entry.max_output_tokens = 65536;
        }
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let mut input = preflight_input("gpt-5");
        input.max_output_tokens_cap = 32768;

        let result = svc.preflight_reserve(input).await.unwrap();
        match result {
            PreflightDecision::Allow {
                max_output_tokens_applied,
                ..
            } => {
                assert_eq!(max_output_tokens_applied, 32768); // config cap wins
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_downgrade_uses_effective_model_max_output() {
        // Task 3.9: downgrade to model with different max_output_tokens
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        // Premium model: max_output_tokens=8192, Standard: max_output_tokens=2048
        for entry in &mut snapshot.model_catalog {
            if entry.tier == ModelTier::Premium {
                entry.max_output_tokens = 8192;
            } else {
                entry.max_output_tokens = 2048;
            }
        }
        snapshot.kill_switches.force_standard_tier = true; // force downgrade
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let mut input = preflight_input("gpt-5");
        input.max_output_tokens_cap = 32768;

        let result = svc.preflight_reserve(input).await.unwrap();
        match result {
            PreflightDecision::Downgrade {
                max_output_tokens_applied,
                effective_model,
                ..
            } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(max_output_tokens_applied, 2048); // standard model's value
            }
            other => panic!("expected Downgrade, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_downgrade_returns_correct_reason() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        snapshot.kill_switches.force_standard_tier = true;
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        match result {
            PreflightDecision::Downgrade {
                effective_model,
                downgrade_from,
                downgrade_reason,
                ..
            } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(downgrade_from, "gpt-5");
                assert_eq!(downgrade_reason, DowngradeReason::ForceStandardTier);
            }
            other => panic!("expected Downgrade, got {other:?}"),
        }
    }

    // 4.7: Downgraded model carries fallback model's context_window, not original's
    #[tokio::test]
    async fn downgraded_model_carries_fallback_context_window() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        // Premium (gpt-5): context_window=128_000, Standard (gpt-5-mini): context_window=64_000
        snapshot.model_catalog[0].context_window = 200_000;
        snapshot.model_catalog[0].max_input_tokens = 190_000;
        snapshot.model_catalog[1].context_window = 64_000;
        snapshot.model_catalog[1].max_input_tokens = 60_000;
        snapshot.kill_switches.force_standard_tier = true; // force downgrade
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        match result {
            PreflightDecision::Downgrade {
                context_window,
                max_input_tokens,
                effective_model,
                ..
            } => {
                assert_eq!(effective_model, "gpt-5-mini");
                assert_eq!(
                    context_window, 64_000,
                    "should use fallback model's context_window"
                );
                assert_eq!(
                    max_input_tokens, 60_000,
                    "should use fallback model's max_input_tokens"
                );
            }
            other => panic!("expected Downgrade, got {other:?}"),
        }
    }

    // 4.8: Per-model EstimationBudgets override flows through PreflightDecision
    #[tokio::test]
    async fn per_model_estimation_budgets_flow_through() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        // Set custom estimation budgets on gpt-5
        snapshot.model_catalog[0].estimation_budgets = mini_chat_sdk::EstimationBudgets {
            bytes_per_token_conservative: 3,
            fixed_overhead_tokens: 200,
            safety_margin_pct: 15,
            image_token_budget: 2000,
            tool_surcharge_tokens: 1000,
            web_search_surcharge_tokens: 800,
            code_interpreter_surcharge_tokens: 1000,
            minimal_generation_floor: 256,
        };
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        match result {
            PreflightDecision::Allow {
                estimation_budgets, ..
            } => {
                assert_eq!(estimation_budgets.bytes_per_token_conservative, 3);
                assert_eq!(estimation_budgets.fixed_overhead_tokens, 200);
                assert_eq!(estimation_budgets.safety_margin_pct, 15);
                assert_eq!(estimation_budgets.image_token_budget, 2000);
                assert_eq!(estimation_budgets.tool_surcharge_tokens, 1000);
                assert_eq!(estimation_budgets.web_search_surcharge_tokens, 800);
                assert_eq!(estimation_budgets.code_interpreter_surcharge_tokens, 1000);
                assert_eq!(estimation_budgets.minimal_generation_floor, 256);
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_reject_returns_429() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let mut limits = default_limits();
        limits.premium.limit_daily_credits_micro = 0;
        limits.standard.limit_daily_credits_micro = 0;
        let svc = TestQuotaService::new(
            db,
            Arc::new(QuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(snapshot)),
            Arc::new(MockUserLimitsProvider::new(limits)),
            crate::config::EstimationBudgets::default(),
            QuotaConfig {
                overshoot_tolerance_factor: 1.10,
                ..QuotaConfig::default()
            },
        );

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        match result {
            PreflightDecision::Reject { http_status, .. } => {
                assert_eq!(http_status, 429);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_premium_reserves_both_buckets() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        assert!(matches!(result, PreflightDecision::Allow { .. }));

        // Verify rows were created for both buckets
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        assert_eq!(
            rows.iter().filter(|r| r.bucket == "total").count(),
            2,
            "total: daily + monthly"
        );
        assert_eq!(
            rows.iter().filter(|r| r.bucket == "tier:premium").count(),
            2,
            "tier:premium: daily + monthly"
        );
        for row in rows
            .iter()
            .filter(|r| r.bucket == "total" || r.bucket == "tier:premium")
        {
            assert!(row.reserved_credits_micro > 0);
        }
    }

    #[tokio::test]
    async fn preflight_standard_reserves_total_only() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let result = svc
            .preflight_reserve(preflight_input("gpt-5-mini"))
            .await
            .unwrap();
        assert!(matches!(result, PreflightDecision::Allow { .. }));

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        assert_eq!(
            rows.iter().filter(|r| r.bucket == "total").count(),
            2,
            "total: daily + monthly"
        );
        assert!(
            !rows.iter().any(|r| r.bucket == "tier:premium"),
            "tier:premium should NOT be reserved"
        );
    }

    // ── preflight_evaluate / preflight_write_reserve tests ──

    #[tokio::test]
    async fn preflight_evaluate_returns_decision_without_writing() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let computed = svc
            .preflight_evaluate(preflight_input("gpt-5"))
            .await
            .unwrap();
        assert!(matches!(computed.decision, PreflightDecision::Allow { .. }));

        // Verify NO rows were written to quota_usage
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();
        assert!(rows.is_empty(), "evaluate must not write quota_usage rows");
    }

    #[tokio::test]
    async fn preflight_write_reserve_increments_buckets() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let computed = svc
            .preflight_evaluate(preflight_input("gpt-5"))
            .await
            .unwrap();

        // Write inside a transaction
        db.transaction(|tx| {
            let svc_repo = Arc::new(QuotaUsageRepo);
            let computed = computed.clone();
            Box::pin(async move {
                let scope = AccessScope::for_tenant(computed.tenant_id);
                use crate::domain::repos::IncrementReserveParams;
                for bucket in &computed.buckets {
                    for (period_type, period_start) in &computed.periods {
                        svc_repo
                            .increment_reserve(
                                tx,
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
                            .await
                            .map_err(to_db)?;
                    }
                }
                Ok(())
            })
        })
        .await
        .unwrap();

        // Verify rows WERE written
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();
        assert!(
            rows.iter().any(|r| r.reserved_credits_micro > 0),
            "write_reserve should have incremented quota_usage"
        );
    }

    // ── Web search preflight tests ──

    #[tokio::test]
    async fn preflight_web_search_kill_switch_rejects() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        snapshot.kill_switches.disable_web_search = true;
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        let mut input = preflight_input("gpt-5");
        input.web_search_enabled = true;

        let result = svc.preflight_evaluate(input).await;
        assert!(
            matches!(result, Err(DomainError::WebSearchDisabled)),
            "expected WebSearchDisabled, got {result:?}"
        );
    }

    #[tokio::test]
    async fn preflight_web_search_daily_quota_rejects() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        // Use a snapshot whose models have web_search capability enabled
        let mut snapshot = default_snapshot();
        for entry in &mut snapshot.model_catalog {
            entry.general_config.tool_support.web_search = true;
        }
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        // Seed daily web search usage at the quota limit
        let today = OffsetDateTime::now_utc().date();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let repo = QuotaUsageRepo;
        let conn = db.conn().unwrap();

        // First create the row via increment_reserve, then settle with web_search_calls
        use crate::domain::repos::IncrementReserveParams;
        repo.increment_reserve(
            &conn,
            &scope,
            IncrementReserveParams {
                tenant_id: Uuid::nil(),
                user_id: Uuid::nil(),
                period_type: PeriodType::Daily,
                period_start: today,
                bucket: "total".to_owned(),
                amount_micro: 1000,
            },
        )
        .await
        .unwrap();

        // Settle with web_search_calls = daily quota (default 75)
        use crate::domain::repos::SettleParams;
        repo.settle(
            &conn,
            &scope,
            SettleParams {
                tenant_id: Uuid::nil(),
                user_id: Uuid::nil(),
                period_type: PeriodType::Daily,
                period_start: today,
                bucket: "total".to_owned(),
                reserved_credits_micro: 1000,
                actual_credits_micro: 1000,
                input_tokens: Some(10),
                output_tokens: Some(5),
                web_search_calls: QuotaConfig::default().web_search_daily_quota,
                code_interpreter_calls: 0,
            },
        )
        .await
        .unwrap();

        let input = preflight_input("gpt-5");

        let computed = svc.preflight_evaluate(input).await.unwrap();
        match computed.decision {
            PreflightDecision::Reject {
                quota_scope,
                http_status,
                ..
            } => {
                assert_eq!(quota_scope, "web_search");
                assert_eq!(http_status, 429);
            }
            other => panic!("expected Reject with web_search scope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_code_interpreter_daily_quota_rejects() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        // Use a snapshot whose models have code_interpreter capability enabled
        let mut snapshot = default_snapshot();
        for entry in &mut snapshot.model_catalog {
            entry.general_config.tool_support.code_interpreter = true;
        }
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);

        // Seed daily code interpreter usage at the quota limit
        let today = OffsetDateTime::now_utc().date();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let repo = QuotaUsageRepo;
        let conn = db.conn().unwrap();

        // First create the row via increment_reserve, then settle with code_interpreter_calls
        use crate::domain::repos::IncrementReserveParams;
        repo.increment_reserve(
            &conn,
            &scope,
            IncrementReserveParams {
                tenant_id: Uuid::nil(),
                user_id: Uuid::nil(),
                period_type: PeriodType::Daily,
                period_start: today,
                bucket: "total".to_owned(),
                amount_micro: 1000,
            },
        )
        .await
        .unwrap();

        // Settle with code_interpreter_calls = daily quota (default 50)
        use crate::domain::repos::SettleParams;
        repo.settle(
            &conn,
            &scope,
            SettleParams {
                tenant_id: Uuid::nil(),
                user_id: Uuid::nil(),
                period_type: PeriodType::Daily,
                period_start: today,
                bucket: "total".to_owned(),
                reserved_credits_micro: 1000,
                actual_credits_micro: 1000,
                input_tokens: Some(10),
                output_tokens: Some(5),
                web_search_calls: 0,
                code_interpreter_calls: QuotaConfig::default().code_interpreter_daily_quota,
            },
        )
        .await
        .unwrap();

        let input = preflight_input("gpt-5");

        let computed = svc.preflight_evaluate(input).await.unwrap();
        match computed.decision {
            PreflightDecision::Reject {
                quota_scope,
                http_status,
                ..
            } => {
                assert_eq!(quota_scope, "code_interpreter");
                assert_eq!(http_status, 429);
            }
            other => panic!("expected Reject with code_interpreter scope, got {other:?}"),
        }
    }

    // ── 10: Integration tests ──

    // 10.2: Full preflight → settle round-trip
    #[tokio::test]
    async fn integration_preflight_settle_roundtrip() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        // Step 1: preflight
        let decision = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        let (
            effective_model,
            reserve_tokens,
            reserved_credits_micro,
            policy_version_applied,
            max_output_tokens_applied,
            minimal_generation_floor_applied,
        ) = match decision {
            PreflightDecision::Allow {
                effective_model,
                reserve_tokens,
                reserved_credits_micro,
                policy_version_applied,
                max_output_tokens_applied,
                minimal_generation_floor_applied,
                ..
            } => (
                effective_model,
                reserve_tokens,
                reserved_credits_micro,
                policy_version_applied,
                max_output_tokens_applied,
                minimal_generation_floor_applied,
            ),
            other => panic!("expected Allow, got {other:?}"),
        };

        // Step 2: settle with actual tokens
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let settle_input = SettlementInput {
            tenant_id: Uuid::nil(),
            user_id: Uuid::nil(),
            effective_model,
            policy_version_applied,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            minimal_generation_floor_applied,
            settlement_path: SettlementPath::Actual {
                input_tokens: 500,
                output_tokens: 200,
            },
            period_starts: default_periods(today),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        };

        let outcome = svc.settle(&conn, &scope, settle_input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);
        assert!(!outcome.overshoot_capped);

        // Step 3: verify DB rows
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        for row in &rows {
            assert!(
                row.spent_credits_micro > 0,
                "spent should be > 0 after settlement"
            );
            assert_eq!(
                row.reserved_credits_micro, 0,
                "reserved should be 0 after settlement"
            );
        }
    }

    // 10.3: Downgrade + settle with standard multipliers
    #[tokio::test]
    async fn integration_downgrade_settle_standard() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let mut snapshot = default_snapshot();
        // Set premium model to 3x multiplier to verify standard is used after downgrade
        snapshot.model_catalog[0].input_tokens_credit_multiplier_micro = 3_000_000;
        snapshot.model_catalog[0].output_tokens_credit_multiplier_micro = 3_000_000;
        snapshot.kill_switches.force_standard_tier = true;

        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        // Step 1: preflight — should downgrade to standard
        let decision = svc
            .preflight_reserve(preflight_input("gpt-5"))
            .await
            .unwrap();
        let (
            effective_model,
            reserve_tokens,
            reserved_credits_micro,
            policy_version_applied,
            max_output_tokens_applied,
            minimal_generation_floor_applied,
        ) = match decision {
            PreflightDecision::Downgrade {
                effective_model,
                reserve_tokens,
                reserved_credits_micro,
                policy_version_applied,
                max_output_tokens_applied,
                minimal_generation_floor_applied,
                downgrade_reason,
                ..
            } => {
                assert_eq!(downgrade_reason, DowngradeReason::ForceStandardTier);
                assert_eq!(effective_model, "gpt-5-mini");
                (
                    effective_model,
                    reserve_tokens,
                    reserved_credits_micro,
                    policy_version_applied,
                    max_output_tokens_applied,
                    minimal_generation_floor_applied,
                )
            }
            other => panic!("expected Downgrade, got {other:?}"),
        };

        // Step 2: settle with standard model
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let settle_input = SettlementInput {
            tenant_id: Uuid::nil(),
            user_id: Uuid::nil(),
            effective_model,
            policy_version_applied,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            minimal_generation_floor_applied,
            settlement_path: SettlementPath::Actual {
                input_tokens: 500,
                output_tokens: 200,
            },
            period_starts: default_periods(today),
            web_search_calls: 0,
            code_interpreter_calls: 0,
        };

        let outcome = svc.settle(&conn, &scope, settle_input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);

        // Step 3: verify only total bucket (not tier:premium)
        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        assert!(
            rows.iter().any(|r| r.bucket == "total"),
            "total bucket should exist"
        );
        assert!(
            !rows.iter().any(|r| r.bucket == "tier:premium"),
            "tier:premium should NOT exist for standard turn"
        );

        // Standard multiplier is 1x, so credits = input + output = 700
        assert_eq!(outcome.actual_credits_micro, 700);
    }

    // 9.7: Standard turn updates total only
    #[tokio::test]
    async fn settle_standard_updates_total_only() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let snapshot = default_snapshot();
        let svc = make_test_service(Arc::clone(&db), snapshot, 1.10);
        let today = OffsetDateTime::now_utc().date();

        seed_reserve(&db, ModelTier::Standard, 10_000, today).await;

        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(Uuid::nil());
        let input = settlement_input(
            "gpt-5-mini",
            ModelTier::Standard,
            2000,
            10_000,
            SettlementPath::Actual {
                input_tokens: 500,
                output_tokens: 500,
            },
            today,
        );

        let outcome = svc.settle(&conn, &scope, input).await.unwrap();
        assert_eq!(outcome.settlement_method, SettlementMethod::Actual);

        use crate::domain::repos::QuotaUsageRepository as QURepo;
        let repo = QuotaUsageRepo;
        let rows = repo
            .find_bucket_rows(&conn, &scope, Uuid::nil(), Uuid::nil())
            .await
            .unwrap();

        assert!(
            rows.iter().any(|r| r.bucket == "total"),
            "total bucket should have rows"
        );
        assert!(
            !rows.iter().any(|r| r.bucket == "tier:premium"),
            "tier:premium should NOT have rows for standard"
        );
    }
}

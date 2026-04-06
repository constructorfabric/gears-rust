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

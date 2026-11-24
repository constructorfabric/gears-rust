use std::num::NonZeroU64;

use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{
    DebitPlanInvariant, EvaluationBudget, EvaluationContext, EvaluationOutcome, EvaluationQuota,
};
use crate::{
    DebitPlan, Decision, EnforcementMode, MetricId, PolicyId, PolicyScope, PolicyVersion,
    PolicyVersionState, QuotaDebitPlan, QuotaId, QuotaScopeTier, QuotaSnapshot, QuotaType,
};

const METRIC: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~";

fn snapshot() -> QuotaSnapshot {
    QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(json!({
            "type": "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~",
            "id": "t-1"
        }))
        .expect("subject"),
        metric: MetricId::parse(METRIC).expect("metric"),
        quota_type: QuotaType::Consumption,
        enforcement_mode: EnforcementMode::Hard,
        cap: None,
        consumed: 0,
        remaining: None,
        period: None,
        metadata: serde_json::Map::new(),
        validity_window: None,
        currently_within_window: true,
    }
}

fn policy(engine_id: &str) -> PolicyVersion {
    PolicyVersion {
        schema_snapshot: crate::PolicySchemaSnapshot::default(),
        policy_id: PolicyId::global(),
        version: 1,
        scope: PolicyScope::Global,
        engine_id: engine_id.into(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".into(),
        comment: None,
    }
}

fn entry(quota: &QuotaSnapshot, amount: u64) -> QuotaDebitPlan {
    QuotaDebitPlan {
        quota_id: quota.quota_id,
        amount,
    }
}

/// Validate `plan` as an `Allowed` decision of `engine_id` for an amount of 50
/// over `quotas`.
fn validate(
    engine_id: &str,
    quotas: &[&QuotaSnapshot],
    plan: DebitPlan,
) -> Result<Decision, DebitPlanInvariant> {
    let policy = policy(engine_id);
    let metric = MetricId::parse(METRIC).expect("metric");
    let quotas: Vec<_> = quotas
        .iter()
        .map(|snapshot| EvaluationQuota {
            snapshot,
            tier: QuotaScopeTier::Tenant,
        })
        .collect();
    let limit = NonZeroU64::new(1000).expect("nonzero");
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric,
        amount: 50,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &quotas,
        request: &Value::Null,
        metric_id: None,
        budget: EvaluationBudget::new(None, limit, limit).expect("budget"),
    };
    EvaluationOutcome::validate(Decision::allowed_with_plan(plan), &context)
        .map(EvaluationOutcome::into_decision)
}

#[test]
fn a_cascade_an_and_and_their_mix_pass_the_boundary() {
    let (user, tenant, device) = (snapshot(), snapshot(), snapshot());
    let all = [&user, &tenant, &device];

    for plan in [
        vec![vec![entry(&user, 20), entry(&tenant, 30)]],
        vec![vec![entry(&user, 50)], vec![entry(&device, 50)]],
        vec![
            vec![entry(&user, 20), entry(&tenant, 30)],
            vec![entry(&device, 50)],
        ],
        vec![vec![entry(&user, 0), entry(&tenant, 50)]],
    ] {
        assert!(validate("cel", &all, plan.clone()).is_ok(), "{plan:?}");
    }
}

#[test]
fn a_chain_that_does_not_add_up_to_the_amount_is_refused() {
    let (user, tenant) = (snapshot(), snapshot());
    let both = [&user, &tenant];

    for plan in [
        vec![vec![entry(&user, 20), entry(&tenant, 20)]],
        vec![vec![entry(&user, 20), entry(&tenant, 40)]],
        vec![vec![entry(&user, 50)], vec![entry(&tenant, 49)]],
        vec![vec![entry(&user, 50)], Vec::new()],
    ] {
        assert_eq!(
            validate("cel", &both, plan.clone()).map(|_| ()),
            Err(DebitPlanInvariant::ChainSumMismatch),
            "{plan:?}"
        );
    }
}

#[test]
fn a_quota_twice_in_a_chain_or_across_chains_is_refused() {
    let (user, tenant) = (snapshot(), snapshot());
    let both = [&user, &tenant];

    for plan in [
        vec![vec![entry(&user, 20), entry(&user, 30)]],
        vec![vec![entry(&user, 50)], vec![entry(&user, 50)]],
        vec![
            vec![entry(&user, 20), entry(&tenant, 30)],
            vec![entry(&tenant, 50)],
        ],
    ] {
        assert_eq!(
            validate("cel", &both, plan.clone()).map(|_| ()),
            Err(DebitPlanInvariant::DuplicateQuota),
            "{plan:?}"
        );
    }
}

#[test]
fn an_allowed_empty_plan_and_a_foreign_quota_are_refused() {
    let (user, stranger) = (snapshot(), snapshot());

    assert_eq!(
        validate("cel", &[&user], Vec::new()).map(|_| ()),
        Err(DebitPlanInvariant::ResultPlanInconsistency)
    );
    assert_eq!(
        validate("cel", &[&user], vec![vec![entry(&stranger, 50)]]).map(|_| ()),
        Err(DebitPlanInvariant::QuotaIdOutsideApplicableSet)
    );
}

#[test]
fn most_restrictive_wins_returns_exactly_one_chain_of_one_entry() {
    let (user, tenant) = (snapshot(), snapshot());
    let both = [&user, &tenant];

    assert!(
        validate(
            "most-restrictive-wins",
            &both,
            vec![vec![entry(&tenant, 50)]]
        )
        .is_ok()
    );
    for plan in [
        vec![vec![entry(&user, 20), entry(&tenant, 30)]],
        vec![vec![entry(&user, 50)], vec![entry(&tenant, 50)]],
    ] {
        assert_eq!(
            validate("most-restrictive-wins", &both, plan.clone()).map(|_| ()),
            Err(DebitPlanInvariant::ResultPlanInconsistency),
            "{plan:?}"
        );
    }
}

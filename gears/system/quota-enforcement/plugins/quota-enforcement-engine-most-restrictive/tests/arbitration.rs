use std::num::NonZeroU64;

use quota_enforcement_engine_most_restrictive::MostRestrictiveWins;
use quota_enforcement_sdk::*;
use serde_json::{Value, json};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const METRIC: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~";
const USER: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~";
const DEVICE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.device.v1~";
const TENANT: &str = "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~";

fn quota(cap: Option<u64>, consumed: u64) -> Result<QuotaSnapshot, Box<dyn std::error::Error>> {
    quota_of(USER, cap, consumed)
}

fn tenant_quota(
    cap: Option<u64>,
    consumed: u64,
) -> Result<QuotaSnapshot, Box<dyn std::error::Error>> {
    quota_of(TENANT, cap, consumed)
}

fn quota_of(
    subject_type: &str,
    cap: Option<u64>,
    consumed: u64,
) -> Result<QuotaSnapshot, Box<dyn std::error::Error>> {
    Ok(QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(json!({"type": subject_type, "id": "alice"}))?,
        metric: MetricId::parse(METRIC)?,
        quota_type: QuotaType::Consumption,
        enforcement_mode: EnforcementMode::Hard,
        cap,
        consumed,
        remaining: cap.map(|value| value.saturating_sub(consumed)),
        period: None,
        metadata: serde_json::Map::new(),
        validity_window: None,
        currently_within_window: true,
    })
}

fn evaluate(
    quotas: &[&QuotaSnapshot],
    amount: u64,
) -> Result<Decision, Box<dyn std::error::Error>> {
    let policy = PolicyVersion {
        schema_snapshot: PolicySchemaSnapshot::default(),
        policy_id: PolicyId::global(),
        version: 1,
        scope: PolicyScope::Global,
        engine_id: "most-restrictive-wins".into(),
        engine_config: json!({}),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".into(),
        comment: None,
    };
    let metric = MetricId::parse(METRIC)?;
    let input: Vec<_> = quotas
        .iter()
        .map(|snapshot| EvaluationQuota {
            snapshot,
            tier: QuotaScopeTier::of(&snapshot.subject),
        })
        .collect();
    let budget = EvaluationBudget::new(
        Some(1000),
        NonZeroU64::new(1000).ok_or("nonzero fixture")?,
        NonZeroU64::new(1000).ok_or("nonzero fixture")?,
    )?;
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric,
        amount,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &input,
        request: &Value::Null,
        metric_id: None,
        budget,
    };
    let artifact = MostRestrictiveWins.validate_config(EngineValidationInput {
        raw: &json!({}),
        schemas: &PolicySchemaSnapshot::default(),
    })?;
    Ok(MostRestrictiveWins.evaluate(&context, artifact.as_ref())?)
}

fn one_entry(quota_id: QuotaId, amount: u64) -> DebitPlan {
    vec![vec![QuotaDebitPlan { quota_id, amount }]]
}

fn charged(decision: &Decision) -> Vec<QuotaId> {
    decision
        .debit_plan
        .iter()
        .flatten()
        .map(|entry| entry.quota_id)
        .collect()
}

#[test]
fn tenant_satisfies_when_user_does_not() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = tenant_quota(Some(10000), 300)?;
    let decision = evaluate(&[&user, &tenant], 50)?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(decision.debit_plan, one_entry(tenant.quota_id, 50));
    Ok(())
}

#[test]
fn tier_precedes_boundedness_and_order_is_irrelevant() -> TestResult {
    let user = quota(None, 0)?;
    let tenant = tenant_quota(Some(50), 0)?;
    let first = evaluate(&[&user, &tenant], 50)?;
    let second = evaluate(&[&tenant, &user], 50)?;
    assert_eq!(first, second);
    assert_eq!(charged(&first), vec![user.quota_id]);
    Ok(())
}

#[test]
fn bounded_then_smallest_remaining_then_id() -> TestResult {
    let unbounded = quota(None, 0)?;
    let large = quota(Some(90), 0)?;
    let a = quota(Some(50), 0)?;
    let b = quota(Some(50), 0)?;
    let result = evaluate(&[&unbounded, &large, &b, &a], 20)?;
    assert_eq!(charged(&result), vec![a.quota_id.min(b.quota_id)]);
    Ok(())
}

#[test]
fn all_violators_are_returned_and_windows_are_inclusive() -> TestResult {
    let mut a = quota(Some(0), 0)?;
    let b = tenant_quota(Some(2), 1)?;
    a.validity_window = Some(ValidityWindow {
        start: Some(OffsetDateTime::UNIX_EPOCH),
        end: Some(OffsetDateTime::UNIX_EPOCH),
    });
    let result = evaluate(&[&a, &b], 5)?;
    let DecisionResult::Denied {
        mut violated_quota_ids,
        ..
    } = result.result
    else {
        panic!("expected denial")
    };
    violated_quota_ids.sort_unstable();
    let mut expected = vec![a.quota_id, b.quota_id];
    expected.sort_unstable();
    assert_eq!(violated_quota_ids, expected);
    assert!(result.debit_plan.is_empty());
    assert_eq!(evaluate(&[&a], 0)?.debit_plan, one_entry(a.quota_id, 0));
    Ok(())
}

#[test]
fn empty_and_expired_sets_deny() -> TestResult {
    let mut expired = quota(None, 0)?;
    expired.validity_window = Some(ValidityWindow {
        start: None,
        end: Some(OffsetDateTime::UNIX_EPOCH - time::Duration::seconds(1)),
    });
    for result in [evaluate(&[], 1)?, evaluate(&[&expired], 1)?] {
        assert_eq!(
            result.result,
            DecisionResult::Denied {
                violated_quota_ids: vec![],
                reason: "NO_APPLICABLE_QUOTA".into()
            }
        );
        assert!(result.debit_plan.is_empty());
    }
    Ok(())
}

#[test]
fn repeated_evaluation_of_one_context_is_byte_identical() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = tenant_quota(Some(10_000), 300)?;
    let inputs = [&user, &tenant];
    // An allowed and a denied verdict, each evaluated twice from one context.
    for amount in [50, 50_000] {
        let first = serde_json::to_vec(&evaluate(&inputs, amount)?)?;
        let second = serde_json::to_vec(&evaluate(&inputs, amount)?)?;
        assert_eq!(first, second, "amount {amount}");
    }
    Ok(())
}

#[test]
fn every_subject_type_but_the_tenant_is_the_subject_tier() -> TestResult {
    let tenant = tenant_quota(Some(10), 0)?;
    let user = quota_of(USER, Some(30), 0)?;
    let device = quota_of(DEVICE, Some(20), 0)?;
    assert_eq!(QuotaScopeTier::of(&tenant.subject), QuotaScopeTier::Tenant);
    assert_eq!(QuotaScopeTier::of(&user.subject), QuotaScopeTier::Subject);
    assert_eq!(QuotaScopeTier::of(&device.subject), QuotaScopeTier::Subject);
    let decision = evaluate(&[&tenant, &user, &device], 5)?;
    assert_eq!(charged(&decision), vec![device.quota_id]);
    let spilled = evaluate(&[&tenant, &user, &device], 25)?;
    assert_eq!(charged(&spilled), vec![user.quota_id]);
    Ok(())
}

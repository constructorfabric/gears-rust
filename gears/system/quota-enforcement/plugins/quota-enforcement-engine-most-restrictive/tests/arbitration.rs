use std::num::NonZeroU64;

use quota_enforcement_engine_most_restrictive::MostRestrictiveWins;
use quota_enforcement_sdk::*;
use serde_json::{Value, json};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn quota(cap: Option<u64>, consumed: u64) -> Result<QuotaSnapshot, Box<dyn std::error::Error>> {
    Ok(QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(
            json!({"projection_type":"gts.cf.core.qe.subj.v1~cf.test.qe.user.v1~", "subject_id":"alice"}),
        )?,
        metric: MetricId::parse("gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1")?,
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
    quotas: &[(&QuotaSnapshot, QuotaScopeTier)],
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
    let metric = MetricId::parse("gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1")?;
    let input: Vec<_> = quotas
        .iter()
        .map(|(snapshot, tier)| EvaluationQuota {
            snapshot,
            tier: *tier,
            arbitration: &Value::Null,
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
        resource: &Value::Null,
        budget,
    };
    let artifact = MostRestrictiveWins.validate_config(EngineValidationInput {
        raw: &json!({}),
        schemas: &PolicySchemaSnapshot::default(),
    })?;
    Ok(MostRestrictiveWins.evaluate(&context, artifact.as_ref())?)
}

#[test]
fn tenant_satisfies_when_user_does_not() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = quota(Some(10000), 300)?;
    let decision = evaluate(
        &[
            (&user, QuotaScopeTier::User),
            (&tenant, QuotaScopeTier::Tenant),
        ],
        50,
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(decision.debit_plan.len(), 1);
    assert_eq!(
        decision.debit_plan.get(&tenant.quota_id),
        Some(&QuotaDebitPlan { amount: 50 })
    );
    Ok(())
}

#[test]
fn tier_precedes_boundedness_and_order_is_irrelevant() -> TestResult {
    let user = quota(None, 0)?;
    let tenant = quota(Some(50), 0)?;
    let first = evaluate(
        &[
            (&user, QuotaScopeTier::User),
            (&tenant, QuotaScopeTier::Tenant),
        ],
        50,
    )?;
    let second = evaluate(
        &[
            (&tenant, QuotaScopeTier::Tenant),
            (&user, QuotaScopeTier::User),
        ],
        50,
    )?;
    assert_eq!(first, second);
    assert!(first.debit_plan.contains_key(&user.quota_id));
    Ok(())
}

#[test]
fn bounded_then_smallest_remaining_then_id() -> TestResult {
    let unbounded = quota(None, 0)?;
    let large = quota(Some(90), 0)?;
    let a = quota(Some(50), 0)?;
    let b = quota(Some(50), 0)?;
    let result = evaluate(
        &[
            (&unbounded, QuotaScopeTier::User),
            (&large, QuotaScopeTier::User),
            (&b, QuotaScopeTier::User),
            (&a, QuotaScopeTier::User),
        ],
        20,
    )?;
    assert_eq!(
        result.debit_plan.keys().copied().collect::<Vec<_>>(),
        vec![a.quota_id.min(b.quota_id)]
    );
    Ok(())
}

#[test]
fn all_violators_are_returned_and_windows_are_inclusive() -> TestResult {
    let mut a = quota(Some(0), 0)?;
    let b = quota(Some(2), 1)?;
    a.validity_window = Some(ValidityWindow {
        start: Some(OffsetDateTime::UNIX_EPOCH),
        end: Some(OffsetDateTime::UNIX_EPOCH),
    });
    let result = evaluate(
        &[(&a, QuotaScopeTier::User), (&b, QuotaScopeTier::Tenant)],
        5,
    )?;
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
    assert_eq!(
        evaluate(&[(&a, QuotaScopeTier::User)], 0)?
            .debit_plan
            .get(&a.quota_id),
        Some(&QuotaDebitPlan { amount: 0 })
    );
    Ok(())
}

#[test]
fn empty_and_expired_sets_deny() -> TestResult {
    let mut expired = quota(None, 0)?;
    expired.validity_window = Some(ValidityWindow {
        start: None,
        end: Some(OffsetDateTime::UNIX_EPOCH - time::Duration::seconds(1)),
    });
    for result in [
        evaluate(&[], 1)?,
        evaluate(&[(&expired, QuotaScopeTier::User)], 1)?,
    ] {
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

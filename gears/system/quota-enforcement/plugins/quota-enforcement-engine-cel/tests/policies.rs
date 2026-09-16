//! Behaviour of the bounded CEL engine against the acceptance scenarios of
//! `features/resolution-policy-engine.md`: save-time diagnostics, the PRD §5.9
//! split cascade and region gating, and the runtime failures that must surface
//! as engine errors rather than a mutated counter.
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use quota_enforcement_engine_cel::{CelEngine, NO_QUOTA_SELECTED};
use quota_enforcement_sdk::*;
use serde_json::{Value, json};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const METRIC: &str = "gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1";
const OTHER_METRIC: &str = "gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.request.v1";

/// One admitted metric: a `region` on the request, `regions` on the Quota.
fn snapshot_for(metric: &str) -> Result<PolicySchemaSnapshot, Box<dyn std::error::Error>> {
    Ok(PolicySchemaSnapshot {
        schemas: BTreeMap::default(),
        inputs: EnvironmentInputs::ALL,
        environments: vec![MetricEnvironmentSchema {
            metric: MetricId::parse(metric)?,
            schema: json!({
                "type": "object",
                "properties": {
                    "request": { "type": "object", "properties": {
                        "region": { "type": "string", "enum": ["eu", "us"] }
                    }},
                    "resource": { "type": "null" },
                    "arbitration": { "type": "object", "properties": {
                        "regions": { "type": "array", "items": { "type": "string", "enum": ["eu", "us"] } },
                        "weight": { "type": "integer" }
                    }}
                }
            }),
        }],
    })
}

fn snapshot() -> Result<PolicySchemaSnapshot, Box<dyn std::error::Error>> {
    snapshot_for(METRIC)
}

fn compile(expr: &str) -> Result<std::sync::Arc<dyn ValidatedConfig>, EngineConfigError> {
    let snapshot = snapshot().map_err(|e| EngineConfigError {
        message: e.to_string(),
        line: None,
        column: None,
    })?;
    CelEngine.validate_config(EngineValidationInput {
        raw: &json!({ "expr": expr }),
        schemas: &snapshot,
    })
}

fn quota(cap: Option<u64>, consumed: u64) -> Result<QuotaSnapshot, Box<dyn std::error::Error>> {
    Ok(QuotaSnapshot {
        quota_id: QuotaId::generate(),
        subject: serde_json::from_value(json!({
            "projection_type": "gts.cf.core.qe.subj.v1~cf.test.qe.user.v1~",
            "subject_id": "alice"
        }))?,
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

struct Scenario<'a> {
    quotas: &'a [(&'a QuotaSnapshot, QuotaScopeTier, Value)],
    amount: u64,
    request: Value,
    cost_limit: u64,
    metric: &'a str,
}

fn evaluate(expr: &str, scenario: &Scenario<'_>) -> Result<Decision, Box<dyn std::error::Error>> {
    let policy = PolicyVersion {
        schema_snapshot: snapshot()?,
        policy_id: PolicyId::global(),
        version: 1,
        scope: PolicyScope::Global,
        engine_id: "cel".into(),
        engine_config: json!({ "expr": expr }),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".into(),
        comment: None,
    };
    let metric = MetricId::parse(scenario.metric)?;
    let input: Vec<_> = scenario
        .quotas
        .iter()
        .map(|(snapshot, tier, arbitration)| EvaluationQuota {
            snapshot,
            tier: *tier,
            arbitration,
        })
        .collect();
    let budget = EvaluationBudget::new(
        Some(1000),
        NonZeroU64::new(1000).ok_or("nonzero fixture")?,
        NonZeroU64::new(scenario.cost_limit).ok_or("nonzero fixture")?,
    )?;
    let context = EvaluationContext {
        policy: &policy,
        metric: &metric,
        amount: scenario.amount,
        time: OffsetDateTime::UNIX_EPOCH,
        quotas: &input,
        request: &scenario.request,
        resource: &Value::Null,
        budget,
    };
    let artifact = compile(expr)?;
    Ok(CelEngine.evaluate(&context, artifact.as_ref())?)
}

fn eu() -> Value {
    json!({ "regions": ["eu"] })
}

/// The PRD §5.9 split cascade: user takes what it has left, tenant the rest.
const SPLIT: &str = r#"
cel.bind(user, quotas.filter(q, q.tier == "user")[0],
cel.bind(tenant, quotas.filter(q, q.tier == "tenant")[0],
cel.bind(first, user.remaining < amount ? user.remaining : amount,
  { "debit_plan": [
      { "id": user.id, "amount": first },
      { "id": tenant.id, "amount": amount - first }
  ] })))"#;

#[test]
fn split_cascade_debits_the_user_remainder_then_the_tenant() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = quota(Some(10_000), 300)?;
    let decision = evaluate(
        SPLIT,
        &Scenario {
            quotas: &[
                (&user, QuotaScopeTier::User, eu()),
                (&tenant, QuotaScopeTier::Tenant, eu()),
            ],
            amount: 50,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
        },
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(
        decision.debit_plan.get(&user.quota_id),
        Some(&QuotaDebitPlan { amount: 20 })
    );
    assert_eq!(
        decision.debit_plan.get(&tenant.quota_id),
        Some(&QuotaDebitPlan { amount: 30 })
    );
    assert_eq!(decision.diagnostics["engine_id"], json!("cel"));
    assert_eq!(
        decision.diagnostics["quotas"][user.quota_id.to_string()]["contribution"],
        json!(20)
    );
    Ok(())
}

const REGION_GATE: &str = r#"
{ "debit_plan": quotas
    .filter(q, request.region in q.arbitration.regions)
    .map(q, { "id": q.id, "amount": amount }) }"#;

#[test]
fn region_gating_debits_only_the_matching_subset() -> TestResult {
    let eu_quota = quota(Some(100), 0)?;
    let us_quota = quota(Some(100), 0)?;
    let scenario = |region: &str| Scenario {
        quotas: &[],
        amount: 7,
        request: json!({ "region": region }),
        cost_limit: 100_000,
        metric: METRIC,
    };
    let quotas = [
        (
            &eu_quota,
            QuotaScopeTier::Tenant,
            json!({ "regions": ["eu"] }),
        ),
        (
            &us_quota,
            QuotaScopeTier::Tenant,
            json!({ "regions": ["us"] }),
        ),
    ];
    let decision = evaluate(
        REGION_GATE,
        &Scenario {
            quotas: &quotas,
            ..scenario("eu")
        },
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(
        decision.debit_plan.keys().copied().collect::<Vec<_>>(),
        vec![eu_quota.quota_id]
    );

    // A predicate that matches nothing is a denial the operator can act on,
    // never `Allowed` with an empty plan.
    let none = evaluate(
        REGION_GATE,
        &Scenario {
            quotas: &[(
                &eu_quota,
                QuotaScopeTier::Tenant,
                json!({ "regions": ["us"] }),
            )],
            ..scenario("eu")
        },
    )?;
    assert_eq!(
        none.result,
        DecisionResult::Denied {
            violated_quota_ids: vec![],
            reason: NO_QUOTA_SELECTED.into()
        }
    );
    assert!(none.debit_plan.is_empty());
    Ok(())
}

#[test]
fn an_explicit_deny_carries_its_reason_and_violators() -> TestResult {
    let q = quota(Some(1), 1)?;
    let decision = evaluate(
        r#"{ "deny": { "reason": "REGION_CLOSED", "violated_quota_ids": quotas.map(q, q.id) } }"#,
        &Scenario {
            quotas: &[(&q, QuotaScopeTier::User, eu())],
            amount: 1,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
        },
    )?;
    assert_eq!(
        decision.result,
        DecisionResult::Denied {
            violated_quota_ids: vec![q.quota_id],
            reason: "REGION_CLOSED".into()
        }
    );
    Ok(())
}

#[test]
fn save_time_rejects_syntax_unknown_fields_and_incompatible_pairs_with_positions() {
    // Syntax: the position points into the operator's source.
    let err = compile("{ \"debit_plan\": [ }")
        .map(|_| ())
        .expect_err("unbalanced");
    assert!(err.message.contains("parse error"), "{err:?}");
    assert_eq!(err.line, Some(1));
    assert!(err.column.is_some());

    // Principal and attribution are not in the environment.
    let err = compile(r#"{ "deny": { "reason": principal.tenant } }"#)
        .map(|_| ())
        .expect_err("principal");
    assert!(err.message.contains("unknown variable"), "{err:?}");
    assert_eq!((err.line, err.column), (Some(1), Some(23)));

    // A property the persisted schema does not declare.
    let err = compile(r#"{ "deny": { "reason": request.zone } }"#)
        .map(|_| ())
        .expect_err("zone");
    assert!(err.message.contains("absent"), "{err:?}");

    // Scalar compared with a collection: the ADR-0007 cardinality check.
    let err = compile(
        r#"{ "debit_plan": quotas.filter(q, request.region == q.arbitration.regions).map(q, { "id": q.id, "amount": amount }) }"#,
    ).map(|_| ()).expect_err("cardinality");
    assert!(err.message.contains("incompatible"), "{err:?}");

    // Paired fields with disjoint declared domains would make the Quota inert.
    let err = compile(
        r#"{ "debit_plan": quotas.filter(q, "apac" in q.arbitration.regions).map(q, { "id": q.id, "amount": amount }) }"#,
    ).map(|_| ()).expect_err("domains");
    assert!(err.message.contains("disjoint"), "{err:?}");

    // Only `expr`, and only a string.
    let snapshot = snapshot().expect("snapshot");
    for raw in [
        json!({}),
        json!({ "expr": 1 }),
        json!({ "expr": "1", "x": 1 }),
        json!([]),
    ] {
        assert!(
            CelEngine
                .validate_config(EngineValidationInput {
                    raw: &raw,
                    schemas: &snapshot
                })
                .is_err(),
            "{raw}"
        );
    }
    let oversized = format!("{{ \"deny\": {{ \"reason\": \"{}\" }} }}", "x".repeat(9000));
    assert!(compile(&oversized).is_err(), "source bound");
    assert!(
        CelEngine
            .validate_config(EngineValidationInput {
                raw: &json!({ "expr": "1" }),
                schemas: &PolicySchemaSnapshot::default(),
            })
            .is_err(),
        "no environment to check against"
    );
}

#[test]
fn a_literal_record_with_the_wrong_shape_is_refused_before_persistence() {
    for expr in [
        "1",
        r#"{ "allow": true }"#,
        r#"{ "debit_plan": 1 }"#,
        r#"{ "debit_plan": [ { "id": 1, "amount": "x" } ] }"#,
        r#"{ "deny": "no" }"#,
    ] {
        assert!(compile(expr).is_err(), "{expr}");
    }
}

#[test]
fn runtime_refuses_negative_duplicate_and_foreign_amounts() -> TestResult {
    let q = quota(Some(100), 0)?;
    let base = |expr: &str| {
        evaluate(
            expr,
            &Scenario {
                quotas: &[(&q, QuotaScopeTier::User, eu())],
                amount: 5,
                request: json!({ "region": "eu" }),
                cost_limit: 100_000,
                metric: METRIC,
            },
        )
    };
    let negative = base(r#"{ "debit_plan": quotas.map(q, { "id": q.id, "amount": -1 }) }"#)
        .expect_err("negative");
    assert!(negative.to_string().contains("negative"), "{negative}");
    let duplicate = base(
        r#"{ "debit_plan": quotas.map(q, { "id": q.id, "amount": 1 }) + quotas.map(q, { "id": q.id, "amount": 1 }) }"#,
    )
    .expect_err("duplicate");
    assert!(duplicate.to_string().contains("twice"), "{duplicate}");
    let foreign =
        base(r#"{ "debit_plan": [ { "id": "not-a-uuid", "amount": 1 } ] }"#).expect_err("uuid");
    assert!(foreign.to_string().contains("UUID"), "{foreign}");
    Ok(())
}

#[test]
fn cost_exhaustion_surfaces_as_an_engine_error() -> TestResult {
    let q = quota(Some(100), 0)?;
    let err = evaluate(
        r#"{ "debit_plan": [1,2,3,4,5,6,7,8].map(a, [1,2,3,4,5,6,7,8].map(b, a * b)).map(l, { "id": quotas[0].id, "amount": 0 }) }"#,
        &Scenario {
            quotas: &[(&q, QuotaScopeTier::User, eu())],
            amount: 5,
            request: json!({ "region": "eu" }),
            cost_limit: 300,
            metric: METRIC,
        },
    )
    .expect_err("budget");
    assert_eq!(err.to_string(), EngineError::CostExceeded.to_string());
    Ok(())
}

#[test]
fn a_metric_outside_the_validated_set_is_refused_not_approximated() -> TestResult {
    let q = quota(Some(100), 0)?;
    let err = evaluate(
        r#"{ "debit_plan": quotas.map(q, { "id": q.id, "amount": amount }) }"#,
        &Scenario {
            quotas: &[(&q, QuotaScopeTier::User, eu())],
            amount: 5,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: OTHER_METRIC,
        },
    )
    .expect_err("other metric");
    assert!(
        err.to_string()
            .contains("outside the policy's validated set"),
        "{err}"
    );
    Ok(())
}

#[test]
fn repeated_evaluation_of_one_context_is_byte_identical() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = quota(Some(10_000), 300)?;
    let scenario = Scenario {
        quotas: &[
            (&user, QuotaScopeTier::User, eu()),
            (&tenant, QuotaScopeTier::Tenant, eu()),
        ],
        amount: 50,
        request: json!({ "region": "eu" }),
        cost_limit: 100_000,
        metric: METRIC,
    };
    let first = serde_json::to_vec(&evaluate(SPLIT, &scenario)?)?;
    let second = serde_json::to_vec(&evaluate(SPLIT, &scenario)?)?;
    assert_eq!(first, second);
    Ok(())
}

#[test]
fn hostile_nesting_and_operator_chains_fail_as_config_errors_not_aborts() {
    // 500 nested parentheses is a kilobyte of source, far under the size bound;
    // without a guard it overflowed the parser's stack and aborted the process.
    let parens = format!("{}1{}", "(".repeat(500), ")".repeat(500));
    let err = compile(&parens).map(|_| ()).expect_err("nesting refused");
    assert!(err.message.contains("nests deeper"), "{err:?}");
    assert_eq!(err.line, Some(1));
    assert!(err.column.is_some());

    // Chains without brackets are what the nesting guard cannot see; they fail
    // inside the parser's own bounded stack instead.
    let nots = format!("{}true", "!".repeat(6000));
    assert!(compile(&nots).map(|_| ()).is_err());
    let ternaries = format!(
        "{}{{ \"deny\": {{ \"reason\": \"x\" }} }}",
        "amount > 1 ? { \"deny\": { \"reason\": \"x\" } } : ".repeat(1500)
    );
    assert!(compile(&ternaries).map(|_| ()).is_err());

    // A bracket inside a string literal is not nesting.
    assert!(
        compile(r#"{ "deny": { "reason": "(((((((((((((((((((((((((((((((((((((" } }"#).is_ok()
    );
}

#[test]
fn every_path_must_produce_a_complete_decision_record() {
    for expr in [
        // an empty literal record
        "{}",
        // an entry without its amount
        r#"{ "debit_plan": [ { "id": quotas[0].id } ] }"#,
        // a second element that disagrees with the first
        r#"{ "debit_plan": [ { "id": quotas[0].id, "amount": 1 }, { "id": quotas[0].id, "amount": true } ] }"#,
        // one valid branch does not vouch for the other
        r#"amount > 1 ? { "debit_plan": [ { "id": quotas[0].id, "amount": 1 } ] } : { "deny": 1 }"#,
        r#"cel.bind(x, 1, amount > 1 ? { "deny": { "reason": "a" } } : { "deny": { "cause": "b" } })"#,
        // an extra field on an entry
        r#"{ "debit_plan": [ { "id": quotas[0].id, "amount": 1, "note": "x" } ] }"#,
        // deny without a reason
        r#"{ "deny": { "violated_quota_ids": [] } }"#,
    ] {
        assert!(compile(expr).map(|_| ()).is_err(), "accepted: {expr}");
    }
    // And the shapes that are legitimate on every path.
    for expr in [
        r#"amount > 1 ? { "debit_plan": [ { "id": quotas[0].id, "amount": 1 } ] } : { "deny": { "reason": "small" } }"#,
        r#"cel.bind(first, quotas[0], { "debit_plan": [ { "id": first.id, "amount": amount } ] })"#,
        r#"{ "debit_plan": [] }"#,
        r#"{ "debit_plan": quotas.map(q, { "id": q.id, "amount": q.remaining < amount ? q.remaining : amount }) }"#,
    ] {
        assert!(compile(expr).is_ok(), "refused: {expr}");
    }
}

#[test]
fn a_field_outside_the_integer_profile_is_reachable_but_unusable() {
    let snapshot = PolicySchemaSnapshot {
        schemas: BTreeMap::default(),
        inputs: EnvironmentInputs::ALL,
        environments: vec![MetricEnvironmentSchema {
            metric: MetricId::parse(METRIC).expect("metric"),
            schema: json!({
                "type": "object",
                "properties": {
                    "request": { "type": "object", "properties": {
                        "rate": { "type": "number" },
                        "region": { "type": "string" },
                        "extra": { "type": "object" }
                    }},
                    "resource": { "type": "null" },
                    "arbitration": { "type": "object", "properties": {
                        "cap": { "type": ["integer", "null"] }
                    }}
                }
            }),
        }],
    };
    let compile = |expr: &str| {
        CelEngine
            .validate_config(EngineValidationInput {
                raw: &json!({ "expr": expr }),
                schemas: &snapshot,
            })
            .map(|_| ())
    };
    // A `number` is not an integer: arithmetic on it is refused at save time
    // rather than failing on the first fractional value at evaluation.
    let err =
        compile(r#"{ "debit_plan": [ { "id": quotas[0].id, "amount": request.rate + 1 } ] }"#)
            .expect_err("number arithmetic");
    assert!(err.message.contains("outside the integer-only"), "{err:?}");
    assert!(compile(r#"{ "deny": { "reason": request.rate > 1 ? "a" : "b" } }"#).is_err());
    // Presence can still be tested, and the rest of the contract stays usable.
    assert!(
        compile(r#"{ "deny": { "reason": has(request.rate) ? request.region : "none" } }"#).is_ok()
    );
    // A nullable integer is an integer operand; evaluation refuses the null.
    assert!(
        compile(r#"{ "debit_plan": quotas.filter(q, q.arbitration.cap > 1).map(q, { "id": q.id, "amount": 1 }) }"#)
            .is_ok()
    );
    // A free-form object's members are untyped and therefore unusable.
    assert!(compile(r#"{ "deny": { "reason": request.extra.note } }"#).is_err());
}

/// The gear persists only the contracts the artifact reports reading; an input
/// it never touched becomes an object with no properties, and the resource slot
/// collapses to `null`.
fn prune(full: &PolicySchemaSnapshot, inputs: EnvironmentInputs) -> PolicySchemaSnapshot {
    let unread = json!({ "type": "object", "properties": {} });
    let mut pruned = full.clone();
    pruned.inputs = inputs;
    for environment in &mut pruned.environments {
        if !inputs.request {
            environment.schema["properties"]["request"] = unread.clone();
        }
        if !inputs.resource {
            environment.schema["properties"]["resource"] = json!({ "type": "null" });
        }
        if !inputs.arbitration {
            environment.schema["properties"]["arbitration"] = unread.clone();
        }
    }
    pruned
}

/// A contract reached through brackets is the same dependency as one reached
/// through a dot. Under-reporting it persists a closure the policy cannot be
/// rebuilt from, so each case walks the whole write path: compile against the
/// full environment, prune to the reported inputs, rebuild from the pruned
/// closure exactly as a restart does.
#[test]
fn a_contract_read_through_brackets_is_reported_and_the_pruned_closure_still_rebuilds() -> TestResult
{
    let cases = [
        (
            r#"{ "debit_plan": quotas.filter(q, size(q['arbitration'].regions) > 0).map(q, { "id": q.id, "amount": amount }) }"#,
            EnvironmentInputs {
                request: false,
                resource: false,
                arbitration: true,
            },
        ),
        (
            r#"{ "deny": { "reason": request['region'] } }"#,
            EnvironmentInputs {
                request: true,
                resource: false,
                arbitration: false,
            },
        ),
        (
            r#"{ "deny": { "reason": request.region + quotas[0]['arbitration']['regions'][0] } }"#,
            EnvironmentInputs {
                request: true,
                resource: false,
                arbitration: true,
            },
        ),
    ];
    for (expr, expected) in cases {
        let artifact = compile(expr)?;
        assert_eq!(artifact.inputs(), expected, "{expr}");
        let pruned = prune(&snapshot()?, expected);
        CelEngine
            .validate_config(EngineValidationInput {
                raw: &json!({ "expr": expr }),
                schemas: &pruned,
            })
            .map_err(|e| format!("{expr} does not rebuild from its own closure: {e}"))?;
    }
    Ok(())
}

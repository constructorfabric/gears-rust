use std::collections::BTreeMap;
use std::num::NonZeroU64;

use quota_enforcement_engine_cel::{CelEngine, NO_QUOTA_SELECTED};
use quota_enforcement_sdk::*;
use serde_json::{Value, json};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const METRIC: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~";
const OTHER_METRIC: &str = "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~";
const USER: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~";
const TENANT: &str = "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~";
const DEVICE: &str = "gts.cf.core.quota.subj.v1~acme.core.quota.device.v1~";

fn metric_type(metadata: &Value) -> Value {
    json!({
        "type": "object",
        "allOf": [
            { "type": "object", "properties": {
                "type": { "type": "string" },
                "id": { "type": "string" },
                "metadata": { "type": "object" }
            }},
            { "type": "object", "properties": { "metadata": metadata } }
        ]
    })
}

fn snapshot_of(metadata: &Value) -> PolicySchemaSnapshot {
    PolicySchemaSnapshot {
        schemas: BTreeMap::from([(METRIC.to_owned(), metric_type(metadata))]),
        inputs: EnvironmentInputs::ALL,
    }
}

fn snapshot() -> PolicySchemaSnapshot {
    snapshot_of(&json!({ "type": "object", "properties": {
        "region": { "type": "string", "enum": ["eu", "us"] }
    }}))
}

fn compile(expr: &str) -> Result<std::sync::Arc<dyn ValidatedConfig>, EngineConfigError> {
    let snapshot = snapshot();
    CelEngine.validate_config(EngineValidationInput {
        raw: &json!({ "expr": expr }),
        schemas: &snapshot,
    })
}

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
        subject: serde_json::from_value(json!({ "type": subject_type, "id": "alice" }))?,
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
    quotas: &'a [&'a QuotaSnapshot],
    amount: u64,
    request: Value,
    cost_limit: u64,
    metric: &'a str,
    metric_id: Option<&'a str>,
}

fn evaluate(expr: &str, scenario: &Scenario<'_>) -> Result<Decision, Box<dyn std::error::Error>> {
    evaluate_under(&snapshot(), expr, scenario)
}

fn evaluate_under(
    schemas: &PolicySchemaSnapshot,
    expr: &str,
    scenario: &Scenario<'_>,
) -> Result<Decision, Box<dyn std::error::Error>> {
    let policy = PolicyVersion {
        schema_snapshot: schemas.clone(),
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
        .map(|snapshot| EvaluationQuota {
            snapshot,
            tier: QuotaScopeTier::of(&snapshot.subject),
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
        metric_id: scenario.metric_id,
        budget,
    };
    let artifact = CelEngine.validate_config(EngineValidationInput {
        raw: &json!({ "expr": expr }),
        schemas,
    })?;
    let decision = CelEngine.evaluate(&context, artifact.as_ref())?;
    Ok(EvaluationOutcome::validate(decision, &context)?.into_decision())
}

fn entry(quota: &QuotaSnapshot, amount: u64) -> QuotaDebitPlan {
    QuotaDebitPlan {
        quota_id: quota.quota_id,
        amount,
    }
}

/// The PRD §5.9 split cascade: user takes what it has left, tenant the rest.
const SPLIT: &str = r#"
cel.bind(user, quotas.filter(q, q.tier == "subject")[0],
cel.bind(tenant, quotas.filter(q, q.tier == "tenant")[0],
cel.bind(first, user.remaining < amount ? user.remaining : amount,
  { "debit_plan": [[
      { "id": user.id, "amount": first },
      { "id": tenant.id, "amount": amount - first }
  ]] })))"#;

#[test]
fn split_cascade_debits_the_user_remainder_then_the_tenant() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = tenant_quota(Some(10_000), 300)?;
    let decision = evaluate(
        SPLIT,
        &Scenario {
            quotas: &[&user, &tenant],
            amount: 50,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(
        decision.debit_plan,
        vec![vec![entry(&user, 20), entry(&tenant, 30)]]
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
    .filter(q, request.region == "eu")
    .map(q, [{ "id": q.id, "amount": amount }]) }"#;

#[test]
fn a_request_predicate_selects_the_quotas_to_debit() -> TestResult {
    let user = quota(Some(100), 0)?;
    let tenant = tenant_quota(Some(100), 0)?;
    let scenario = |region: &str| Scenario {
        quotas: &[],
        amount: 7,
        request: json!({ "region": region }),
        cost_limit: 100_000,
        metric: METRIC,
        metric_id: None,
    };
    let quotas = [&user, &tenant];
    let decision = evaluate(
        REGION_GATE,
        &Scenario {
            quotas: &quotas,
            ..scenario("eu")
        },
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(
        decision.debit_plan,
        vec![vec![entry(&user, 7)], vec![entry(&tenant, 7)]]
    );

    // A predicate that matches nothing is a denial the operator can act on,
    // never `Allowed` with an empty plan.
    let none = evaluate(
        REGION_GATE,
        &Scenario {
            quotas: &quotas,
            ..scenario("us")
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
            quotas: &[&q],
            amount: 1,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
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

    // There is no resource.
    let err = compile(r#"{ "deny": { "reason": resource.metadata.kind } }"#)
        .map(|_| ())
        .expect_err("resource");
    assert!(err.message.contains("unknown variable"), "{err:?}");

    // Paired request fields with disjoint declared domains would make a Quota inert.
    let err = compile(
        r#"{ "debit_plan": quotas.filter(q, request.region == "apac").map(q, [{ "id": q.id, "amount": amount }]) }"#,
    ).map(|_| ()).expect_err("domains");
    assert!(err.message.contains("disjoint"), "{err:?}");

    // Only `expr`, and only a string.
    let snapshot = snapshot();
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
}

#[test]
fn a_literal_record_with_the_wrong_shape_is_refused_before_persistence() {
    for expr in [
        "1",
        r#"{ "allow": true }"#,
        r#"{ "debit_plan": 1 }"#,
        r#"{ "debit_plan": [[ { "id": 1, "amount": "x" } ]] }"#,
        r#"{ "deny": "no" }"#,
    ] {
        assert!(compile(expr).is_err(), "{expr}");
    }
}

#[test]
fn an_and_debits_the_whole_amount_from_every_chain() -> TestResult {
    let user = quota(Some(100), 0)?;
    let device = quota(Some(100), 0)?;
    let decision = evaluate(
        r#"{ "debit_plan": quotas.map(q, [{ "id": q.id, "amount": amount }]) }"#,
        &Scenario {
            quotas: &[&user, &device],
            amount: 50,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )?;
    assert_eq!(
        decision.debit_plan,
        vec![vec![entry(&user, 50)], vec![entry(&device, 50)]]
    );
    Ok(())
}

/// A user-to-tenant cascade, and the device cap counting the whole operation.
const MIX: &str = r#"
cel.bind(user, quotas.filter(q, q.subject_type == "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~")[0],
cel.bind(tenant, quotas.filter(q, q.tier == "tenant")[0],
cel.bind(device, quotas.filter(q, q.subject_type == "gts.cf.core.quota.subj.v1~acme.core.quota.device.v1~")[0],
cel.bind(first, user.remaining < amount ? user.remaining : amount,
  { "debit_plan": [
      [ { "id": user.id, "amount": first }, { "id": tenant.id, "amount": amount - first } ],
      [ { "id": device.id, "amount": amount } ]
  ] }))))"#;

#[test]
fn a_mix_cascades_one_chain_and_ands_another() -> TestResult {
    let user = quota(Some(100), 80)?;
    let tenant = tenant_quota(Some(10_000), 0)?;
    let device = quota_of(DEVICE, Some(100), 0)?;
    let decision = evaluate(
        MIX,
        &Scenario {
            quotas: &[&user, &tenant, &device],
            amount: 50,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )?;
    assert_eq!(
        decision.debit_plan,
        vec![
            vec![entry(&user, 20), entry(&tenant, 30)],
            vec![entry(&device, 50)]
        ]
    );
    Ok(())
}

#[test]
fn a_flat_plan_is_refused_at_save_time_and_at_evaluation() -> TestResult {
    let err = compile(r#"{ "debit_plan": quotas.map(q, { "id": q.id, "amount": amount }) }"#)
        .map(|_| ())
        .expect_err("a computed flat plan");
    assert!(err.message.contains("list of chains"), "{err:?}");
    let err = compile(r#"{ "debit_plan": [ { "id": quotas[0].id, "amount": amount } ] }"#)
        .map(|_| ())
        .expect_err("a literal flat plan");
    assert!(err.message.contains("list of chains"), "{err:?}");

    let q = quota(Some(100), 0)?;
    let flat = json!({ "plan": [ { "id": q.quota_id, "amount": 5 } ] });
    let err = evaluate_under(
        &PolicySchemaSnapshot::default(),
        r#"{ "debit_plan": request.plan }"#,
        &Scenario {
            quotas: &[&q],
            amount: 5,
            request: flat,
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )
    .expect_err("an untyped flat plan");
    assert!(err.to_string().contains("list of chains"), "{err}");
    Ok(())
}

#[test]
fn a_plan_breaking_the_chain_rules_is_refused_at_the_boundary() -> TestResult {
    let user = quota(Some(100), 0)?;
    let tenant = tenant_quota(Some(100), 0)?;
    let scenario = |expr: &str| {
        evaluate(
            expr,
            &Scenario {
                quotas: &[&user, &tenant],
                amount: 5,
                request: json!({ "region": "eu" }),
                cost_limit: 100_000,
                metric: METRIC,
                metric_id: None,
            },
        )
    };
    let short = scenario(
        r#"{ "debit_plan": [[ { "id": quotas[0].id, "amount": 1 }, { "id": quotas[1].id, "amount": 1 } ]] }"#,
    )
    .expect_err("a chain that does not add up");
    assert!(short.to_string().contains("chain_sum_mismatch"), "{short}");
    let twice = scenario(
        r#"{ "debit_plan": quotas.map(q, [{ "id": q.id, "amount": amount }]) + quotas.map(q, [{ "id": q.id, "amount": amount }]) }"#,
    )
    .expect_err("a quota in two chains");
    assert!(twice.to_string().contains("duplicate_quota"), "{twice}");
    Ok(())
}

#[test]
fn a_plan_repeating_one_quota_with_huge_amounts_reports_the_duplicate() -> TestResult {
    let q = quota(Some(100), 0)?;
    let huge = r#"[{ "id": quotas[0].id, "amount": amount }]"#;
    let err = evaluate(
        &format!(r#"{{ "debit_plan": [{huge}, {huge}, {huge}] }}"#),
        &Scenario {
            quotas: &[&q],
            amount: i64::MAX.unsigned_abs(),
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )
    .expect_err("a quota in three chains");
    assert!(err.to_string().contains("duplicate_quota"), "{err}");
    Ok(())
}

#[test]
fn runtime_refuses_negative_and_foreign_amounts() -> TestResult {
    let q = quota(Some(100), 0)?;
    let base = |expr: &str| {
        evaluate(
            expr,
            &Scenario {
                quotas: &[&q],
                amount: 5,
                request: json!({ "region": "eu" }),
                cost_limit: 100_000,
                metric: METRIC,
                metric_id: None,
            },
        )
    };
    let negative = base(r#"{ "debit_plan": quotas.map(q, [{ "id": q.id, "amount": -1 }]) }"#)
        .expect_err("negative");
    assert!(negative.to_string().contains("negative"), "{negative}");
    let foreign =
        base(r#"{ "debit_plan": [[ { "id": "not-a-uuid", "amount": 1 } ]] }"#).expect_err("uuid");
    assert!(foreign.to_string().contains("UUID"), "{foreign}");
    Ok(())
}

#[test]
fn cost_exhaustion_surfaces_as_an_engine_error() -> TestResult {
    let q = quota(Some(100), 0)?;
    let err = evaluate(
        r#"{ "debit_plan": [1,2,3,4,5,6,7,8].map(a, [1,2,3,4,5,6,7,8].map(b, a * b)).map(l, [{ "id": quotas[0].id, "amount": 0 }]) }"#,
        &Scenario {
            quotas: &[&q],
            amount: 5,
            request: json!({ "region": "eu" }),
            cost_limit: 300,
            metric: METRIC,
            metric_id: None,
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
        r#"{ "debit_plan": quotas.map(q, [{ "id": q.id, "amount": amount }]) }"#,
        &Scenario {
            quotas: &[&q],
            amount: 5,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: OTHER_METRIC,
            metric_id: None,
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
    let tenant = tenant_quota(Some(10_000), 300)?;
    let scenario = Scenario {
        quotas: &[&user, &tenant],
        amount: 50,
        request: json!({ "region": "eu" }),
        cost_limit: 100_000,
        metric: METRIC,
        metric_id: None,
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
        r#"{ "debit_plan": [[ { "id": quotas[0].id } ]] }"#,
        // a second element that disagrees with the first
        r#"{ "debit_plan": [[ { "id": quotas[0].id, "amount": 1 }, { "id": quotas[0].id, "amount": true } ]] }"#,
        // one valid branch does not vouch for the other
        r#"amount > 1 ? { "debit_plan": [[ { "id": quotas[0].id, "amount": 1 } ]] } : { "deny": 1 }"#,
        r#"cel.bind(x, 1, amount > 1 ? { "deny": { "reason": "a" } } : { "deny": { "cause": "b" } })"#,
        // an extra field on an entry
        r#"{ "debit_plan": [[ { "id": quotas[0].id, "amount": 1, "note": "x" } ]] }"#,
        // deny without a reason
        r#"{ "deny": { "violated_quota_ids": [] } }"#,
    ] {
        assert!(compile(expr).map(|_| ()).is_err(), "accepted: {expr}");
    }
    // And the shapes that are legitimate on every path.
    for expr in [
        r#"amount > 1 ? { "debit_plan": [[ { "id": quotas[0].id, "amount": 1 } ]] } : { "deny": { "reason": "small" } }"#,
        r#"cel.bind(first, quotas[0], { "debit_plan": [[ { "id": first.id, "amount": amount } ]] })"#,
        r#"{ "debit_plan": [] }"#,
        r#"{ "debit_plan": quotas.map(q, [{ "id": q.id, "amount": q.remaining < amount ? q.remaining : amount }]) }"#,
    ] {
        assert!(compile(expr).is_ok(), "refused: {expr}");
    }
}

#[test]
fn a_field_outside_the_integer_profile_is_reachable_but_unusable() {
    let snapshot = snapshot_of(&json!({ "type": "object", "properties": {
        "rate": { "type": "number" },
        "region": { "type": "string" },
        "extra": { "type": "object" },
        "limit": { "type": ["integer", "null"] }
    }}));
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
        compile(r#"{ "debit_plan": [[ { "id": quotas[0].id, "amount": request.rate + 1 } ]] }"#)
            .expect_err("number arithmetic");
    assert!(err.message.contains("outside the integer-only"), "{err:?}");
    assert!(compile(r#"{ "deny": { "reason": request.rate > 1 ? "a" : "b" } }"#).is_err());
    // Presence can still be tested, and the rest of the contract stays usable.
    assert!(
        compile(r#"{ "deny": { "reason": has(request.rate) ? request.region : "none" } }"#).is_ok()
    );
    // A nullable integer is an integer operand; evaluation refuses the null.
    assert!(
        compile(r#"{ "debit_plan": quotas.filter(q, request.limit > 1).map(q, [{ "id": q.id, "amount": 1 }]) }"#)
            .is_ok()
    );
    // A free-form object's members are untyped and therefore unusable.
    assert!(compile(r#"{ "deny": { "reason": request.extra.note } }"#).is_err());
}

#[test]
fn inputs_read_through_brackets_are_reported() -> TestResult {
    let cases = [
        (
            r#"{ "debit_plan": quotas.filter(q, q['tier'] == "subject").map(q, [{ "id": q.id, "amount": amount }]) }"#,
            EnvironmentInputs { request: false },
        ),
        (
            r#"{ "deny": { "reason": request['region'] } }"#,
            EnvironmentInputs { request: true },
        ),
    ];
    for (expr, expected) in cases {
        assert_eq!(compile(expr)?.inputs(), expected, "{expr}");
    }
    Ok(())
}

#[test]
fn a_quota_carries_no_metadata() {
    for expr in [
        r#"{ "deny": { "reason": quotas[0].arbitration.region } }"#,
        r#"{ "deny": { "reason": quotas[0]['metadata']['region'] } }"#,
    ] {
        assert!(compile(expr).is_err(), "accepted: {expr}");
    }
}

#[test]
fn each_quota_exposes_its_subject_type_and_tier() -> TestResult {
    let user = quota(Some(100), 0)?;
    let tenant = tenant_quota(Some(100), 0)?;
    let decision = evaluate(
        &format!(
            r#"{{ "debit_plan": quotas.filter(q, q.subject_type == "{USER}" && q.tier == "subject").map(q, [{{ "id": q.id, "amount": amount }}]) }}"#
        ),
        &Scenario {
            quotas: &[&tenant, &user],
            amount: 3,
            request: json!({ "region": "eu" }),
            cost_limit: 100_000,
            metric: METRIC,
            metric_id: None,
        },
    )?;
    assert_eq!(decision.debit_plan, vec![vec![entry(&user, 3)]]);
    Ok(())
}

#[test]
fn a_policy_reads_the_id_of_the_requests_metric() -> TestResult {
    let expr = r#"metric_id == "blocked" ? { "deny": { "reason": "blocked" } } : { "debit_plan": [[{ "id": quotas[0].id, "amount": amount }]] }"#;
    let q = quota(Some(100), 0)?;
    for (metric_id, allowed) in [
        (Some("blocked"), false),
        (Some("other"), true),
        (None, true),
    ] {
        let decision = evaluate(
            expr,
            &Scenario {
                quotas: &[&q],
                amount: 2,
                request: json!({}),
                cost_limit: 100_000,
                metric: METRIC,
                metric_id,
            },
        )?;
        assert_eq!(
            decision.result == DecisionResult::Allowed,
            allowed,
            "{metric_id:?}"
        );
    }
    Ok(())
}

#[test]
fn an_empty_snapshot_leaves_request_untyped_and_serves_any_metric() -> TestResult {
    let global = PolicySchemaSnapshot::default();
    let expr = r#"{ "debit_plan": quotas.filter(q, request.zone == "eu").map(q, [{ "id": q.id, "amount": amount }]) }"#;
    let q = quota(Some(100), 0)?;
    for metric in [METRIC, OTHER_METRIC] {
        let decision = evaluate_under(
            &global,
            expr,
            &Scenario {
                quotas: &[&q],
                amount: 2,
                request: json!({ "zone": "eu" }),
                cost_limit: 100_000,
                metric,
                metric_id: None,
            },
        )?;
        assert_eq!(decision.result, DecisionResult::Allowed, "{metric}");
    }
    Ok(())
}

/// One misuse per type rule: each is refused at save time, and the message
/// names the rule it broke rather than a generic failure.
#[test]
fn each_type_rule_refuses_its_own_misuse() {
    const PLAN: &str = r#"{ "debit_plan": [] }"#;
    let gated = |cond: &str| format!("{cond} ? {PLAN} : {PLAN}");
    let cases = [
        (
            gated(r#"amount < "x""#),
            "comparison requires matching scalar types",
        ),
        (
            gated(r#"amount + "x" > 1"#),
            "arithmetic requires integer operands",
        ),
        (
            gated("1 in amount"),
            "membership requires a compatible collection",
        ),
        (gated("size(amount) > 1"), "outside the bounded CEL profile"),
        (gated(r#"-"x" > 1"#), "unary operator type mismatch"),
        (gated("amount && true"), "logical operands must be boolean"),
        (gated("amount"), "condition must be boolean"),
        (
            gated("amount.x > 1"),
            "property access requires a typed object",
        ),
        (gated("amount[0] > 1"), "index type mismatch"),
        (
            gated("request[request.region] > 1"),
            "dynamic object indexing",
        ),
        (
            gated("size([1, \"a\"]) > 1"),
            "list elements have incompatible types",
        ),
        (gated("size({1: 2}) > 1"), "map key must be a string"),
        (gated(r#"size({"a": 1, "a": 2}) > 1"#), "duplicate map key"),
        (
            gated("amount.all(x, true)"),
            "comprehension requires collection",
        ),
        (gated("1.5 > 1"), "outside the bounded integer CEL profile"),
        (
            r#"{ "debit_plan": amount > 1 ? [] : 1 }"#.to_owned(),
            "conditional branches must have compatible types",
        ),
        (
            r#"{ "debit_plan": [], "deny": { "reason": "x" } }"#.to_owned(),
            "carries both",
        ),
        (
            r#"{ "debit_plan": [1] }"#.to_owned(),
            "list of chains of `{id, amount}` records",
        ),
    ];
    for (expr, expected) in cases {
        let err = compile(&expr).map(|_| ()).expect_err(&expr);
        assert!(err.message.contains(expected), "{expr}: {err:?}");
    }
}

/// The bounded functions and collection forms the profile does admit.
#[test]
fn the_bounded_profile_admits_its_own_functions_and_collections() {
    const PLAN: &str = r#"{ "debit_plan": [] }"#;
    for cond in [
        r#""region" in request"#,
        r#"request.region.contains("e")"#,
        r#"request.region.startsWith("e") || request.region.endsWith("u")"#,
        r#"int("1") > 0"#,
        r#"string(1) == "1""#,
        "size(quotas) > 0",
        "size([null]) > 0",
        "!(amount > 1)",
        "-amount < 0",
    ] {
        let expr = format!("{cond} ? {PLAN} : {PLAN}");
        assert!(compile(&expr).is_ok(), "refused: {expr}");
    }
}

/// The gear's default `evaluation_cost_limit`.
const SHIPPED_COST_LIMIT: u64 = 10_000;

#[test]
fn the_prd_policies_fit_the_shipped_cost_limit_with_a_realistic_quota_set() -> TestResult {
    let owned: Vec<QuotaSnapshot> = (0..20)
        .map(|i| quota_of(if i == 0 { USER } else { TENANT }, Some(10_000), i * 10))
        .collect::<Result<_, _>>()?;
    let quotas: Vec<_> = owned.iter().collect();
    for (name, expr) in [("split", SPLIT), ("region gate", REGION_GATE)] {
        let decision = evaluate(
            expr,
            &Scenario {
                quotas: &quotas,
                amount: 50,
                request: json!({ "region": "eu" }),
                cost_limit: SHIPPED_COST_LIMIT,
                metric: METRIC,
                metric_id: None,
            },
        )
        .map_err(|e| format!("{name}: {e}"))?;
        assert_eq!(decision.result, DecisionResult::Allowed, "{name}");
    }
    Ok(())
}

/// Twenty quotas, the realistic upper end of an applicable set.
fn heavy_quotas() -> Result<Vec<QuotaSnapshot>, Box<dyn std::error::Error>> {
    (0..20)
        .map(|i| quota_of(if i == 0 { USER } else { TENANT }, Some(10_000), 0))
        .collect()
}

#[test]
fn a_large_quota_set_binds_within_the_shipped_cost_limit() -> TestResult {
    let owned = heavy_quotas()?;
    let quotas: Vec<_> = owned.iter().collect();
    let decision = evaluate(
        r#"{ "debit_plan": [[ { "id": quotas[0].id, "amount": amount } ]] }"#,
        &Scenario {
            quotas: &quotas,
            amount: 5,
            request: json!({ "region": "eu" }),
            cost_limit: SHIPPED_COST_LIMIT,
            metric: METRIC,
            metric_id: None,
        },
    )?;
    assert_eq!(decision.result, DecisionResult::Allowed);
    Ok(())
}

#[test]
fn the_prd_policies_filter_a_large_quota_set_within_the_shipped_cost_limit() -> TestResult {
    let owned = heavy_quotas()?;
    let quotas: Vec<_> = owned.iter().collect();
    for (name, expr, debited) in [("split", SPLIT, 2), ("region gate", REGION_GATE, 20)] {
        let decision = evaluate(
            expr,
            &Scenario {
                quotas: &quotas,
                amount: 5,
                request: json!({ "region": "eu" }),
                cost_limit: SHIPPED_COST_LIMIT,
                metric: METRIC,
                metric_id: None,
            },
        )
        .map_err(|e| format!("{name}: {e}"))?;
        assert_eq!(decision.result, DecisionResult::Allowed, "{name}");
        assert_eq!(
            decision.debit_plan.iter().flatten().count(),
            debited,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn a_rejected_deep_expression_is_refused_on_a_small_caller_stack() -> TestResult {
    // A bracket-free operator chain fills the source bound; the rejected tree
    // must not be dropped recursively on the caller's stack.
    let expr = format!("{{ \"allow\": {}true }}", "!".repeat(8000));
    let refused = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || compile(&expr).err().map(|e| e.message))?
        .join()
        .map_err(|_| "the compile thread panicked")?;
    let message = refused.ok_or("a chain past the AST bounds was accepted")?;
    assert!(message.contains("AST bounds"), "{message}");
    Ok(())
}

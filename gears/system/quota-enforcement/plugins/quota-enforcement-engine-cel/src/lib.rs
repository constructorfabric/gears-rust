//! Bounded CEL quota resolution engine (`engine_id = "cel"`).
//!
//! `CAPABILITIES.md` records why this crate does not call an off-the-shelf CEL
//! interpreter: neither inspected Rust runtime exposes an internal deadline or
//! cost hook, and an engine evaluates while the evaluation transaction holds
//! Quota rows. `cel-core` supplies the parser and macro expansion; type checking
//! and evaluation are QE's own bounded profile over that AST, metered through
//! the SDK's [`EvaluationBudget`](quota_enforcement_sdk::EvaluationBudget) so a
//! runaway expression fails inside the budget rather than being preempted.
//!
//! # Environment
//!
//! An expression sees three variables, typed at save time from the policy's
//! persisted schema snapshot and bound at evaluation from the
//! [`EvaluationContext`]:
//!
//! | Variable   | Type                                         | Source                     |
//! |------------|----------------------------------------------|----------------------------|
//! | `request`  | the `metadata` schema of the metric type     | the request's metric metadata |
//! | `amount`   | `int`                                        | the requested debit        |
//! | `metric_id`| `string`                                     | the `id` of the request's metric object, `""` when it has none |
//! | `quotas`   | `list<Quota>`                                | every applicable Quota     |
//!
//! `Quota` is `{ id: string, subject_type: string, tier: "subject" | "tenant",
//! cap, consumed: int, remaining }`. A Quota carries no metadata: QE core has
//! already left out the Quotas whose conditions do not hold on the request
//! (ADR-0013). A snapshot with no metric type schema leaves `request` untyped,
//! so every operation on it is checked at evaluation.
//! `cap` and `remaining` are `null` for an unbounded Quota. Principal and
//! attribution are absent from the environment and therefore rejected as
//! unknown variables at save time.
//!
//! # Decision record
//!
//! The expression returns exactly one of:
//!
//! * `{ "debit_plan": [ [ { "id": string, "amount": int }, ... ], ... ] }` —
//!   `Allowed` with that plan, one inner list per chain, primary pool first. A
//!   plan that selects no Quota is `Denied` with reason `NO_QUOTA_SELECTED`,
//!   never `Allowed` with an empty plan. Negative amounts are rejected before
//!   any conversion.
//! * `{ "deny": { "reason": string, "violated_quota_ids": [string, ...] } }` —
//!   `Denied`; `violated_quota_ids` may be omitted.
//!
//! The SDK boundary then applies the closed Debit-Plan invariants.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod check;
mod runtime;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use cel_core::types::SpannedExpr;
use quota_enforcement_sdk::{
    DebitPlan, Decision, DecisionResult, EngineConfigError, EngineError, EngineValidationInput,
    EvaluationContext, PolicySchemaSnapshot, QuotaDebitPlan, QuotaId, QuotaResolutionEngineV1,
    QuotaScopeTier, ValidatedConfig,
};
use serde_json::{Map, Value, json};

use check::{Checker, Kind, Shape};
use quota_enforcement_sdk::EnvironmentInputs;
use runtime::{Runtime, cost};

/// The registered identifier of this engine.
pub const ENGINE_ID: &str = "cel";

/// Longest expression source accepted, in bytes. The AST node and depth
/// bounds in `check` apply after macro expansion, which this bound keeps from
/// being fed an arbitrarily large input in the first place.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024;

/// Reason of the `Denied` a plan that selected no Quota is normalised to.
pub const NO_QUOTA_SELECTED: &str = "NO_QUOTA_SELECTED";

/// Deepest bracket nesting accepted before the parser runs. `cel-core`'s
/// parser is recursive descent with no limit of its own, and 500 nested
/// parentheses fit in a kilobyte, so the source bound alone does not protect
/// the stack.
pub const MAX_NESTING: usize = 32;

/// Stack the parser runs on. Bracket nesting is bounded above, but operator
/// chains (`!!!x`, `a ? b : c ? d : e`) recurse per operator without a
/// bracket, and an 8 KiB source can hold thousands; this leaves them room to
/// fail cleanly instead of aborting the process.
const PARSER_STACK_BYTES: usize = 64 << 20;

/// The statically linked CEL engine.
#[derive(Debug, Default)]
pub struct CelEngine;

quota_enforcement_sdk::register_engine!(|| Arc::new(CelEngine));

/// The immutable compiled artifact: the expanded AST and the metric types it
/// was checked against, empty when it applies to any metric. Persisted policies
/// keep the source and schemas, never this struct; a cache miss rebuilds it
/// through [`QuotaResolutionEngineV1::validate_config`].
struct Artifact {
    source: Arc<str>,
    expr: SpannedExpr,
    metrics: Vec<String>,
    inputs: EnvironmentInputs,
}

impl ValidatedConfig for Artifact {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn inputs(&self) -> EnvironmentInputs {
        self.inputs
    }
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-cel-engine:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-cel-engine:p1
impl QuotaResolutionEngineV1 for CelEngine {
    fn id(&self) -> &'static str {
        ENGINE_ID
    }

    fn validate_config(
        &self,
        input: EngineValidationInput<'_>,
    ) -> Result<Arc<dyn ValidatedConfig>, EngineConfigError> {
        let source = expression_source(input.raw)?;
        guard_nesting(source)?;
        Ok(Arc::new(compile_on_parser_stack(source, input.schemas)?))
    }

    fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
        config: &dyn ValidatedConfig,
    ) -> Result<Decision, EngineError> {
        let artifact = config.as_any().downcast_ref::<Artifact>().ok_or_else(|| {
            EngineError::InvalidConfig("artifact belongs to another engine".into())
        })?;
        if !artifact.metrics.is_empty()
            && !artifact
                .metrics
                .iter()
                .any(|m| m == context.metric.as_str())
        {
            // No fallback to another engine or a looser environment: the
            // expression was never type-checked for this metric.
            return Err(EngineError::InvalidConfig(format!(
                "metric {} is outside the policy's validated set",
                context.metric
            )));
        }
        let mut runtime = Runtime {
            meter: context.budget.start(),
            variables: BTreeMap::new(),
        };
        // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-error-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-error
        // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-evaluate
        bind_environment(&mut runtime, context)?;
        let record = runtime.eval(&artifact.expr)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-evaluate
        // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-error
        // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-error-if
        let _ = &artifact.source;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-decision
        // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-return
        decode(record, context)
        // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-return
        // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-decision
    }
}

/// The `expr` string out of `{ "expr": "<CEL>" }`, and nothing else.
fn expression_source(raw: &Value) -> Result<&str, EngineConfigError> {
    let object = raw
        .as_object()
        .ok_or_else(|| check::config_error("cel configuration must be an object"))?;
    if object.len() != 1 {
        return Err(check::config_error(
            "cel configuration carries exactly one key, `expr`",
        ));
    }
    let source = object
        .get("expr")
        .and_then(Value::as_str)
        .ok_or_else(|| check::config_error("cel configuration needs a string `expr`"))?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err(check::config_error(&format!(
            "expression exceeds {MAX_SOURCE_BYTES} bytes"
        )));
    }
    Ok(source)
}

/// The typed variables of an environment whose `request` has `request` shape.
fn environment_variables(request: Shape) -> BTreeMap<String, Shape> {
    let mut quota = BTreeMap::new();
    quota.insert("id".to_owned(), Shape::new(Kind::String));
    quota.insert("subject_type".to_owned(), Shape::new(Kind::String));
    quota.insert(
        "tier".to_owned(),
        Shape {
            kind: Kind::String,
            domain: Some(vec![json!("subject"), json!("tenant")]),
        },
    );
    quota.insert("consumed".to_owned(), Shape::new(Kind::Int));
    // `null` for an unbounded Quota, so neither is statically an int; a
    // comparison against one is checked at evaluation instead.
    quota.insert("cap".to_owned(), Shape::new(Kind::OptionalInt));
    quota.insert("remaining".to_owned(), Shape::new(Kind::OptionalInt));

    let mut variables = BTreeMap::new();
    variables.insert("request".to_owned(), request);
    variables.insert("amount".to_owned(), Shape::new(Kind::Int));
    variables.insert("metric_id".to_owned(), Shape::new(Kind::String));
    variables.insert(
        "quotas".to_owned(),
        Shape::list(Shape::new(Kind::Object(quota))),
    );
    variables
}

/// The shape of the `metadata` property of a resolved metric type schema.
fn request_shape(metric_type: &Value) -> Result<Shape, EngineConfigError> {
    match check::schema(metric_type, 0)?.kind {
        Kind::Object(mut fields) => fields
            .remove("metadata")
            .ok_or_else(|| check::config_error("metric type schema declares no `metadata`")),
        _ => Err(check::config_error(
            "metric type schema must describe an object",
        )),
    }
}

/// Refuse bracket nesting deeper than [`MAX_NESTING`] before the recursive
/// parser sees the source. String literals are skipped so a bracket inside one
/// does not count.
fn guard_nesting(source: &str) -> Result<(), EngineConfigError> {
    let mut depth = 0_usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, ch) in source.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' | '[' | '{' => {
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(check::at(
                        source,
                        offset,
                        &format!("expression nests deeper than {MAX_NESTING} levels"),
                    ));
                }
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Parse and check on the parser's own generously sized stack, so an operator
/// chain the nesting guard cannot see through fails cleanly rather than
/// aborting the process. Only an accepted tree, which the checker bounds,
/// leaves the thread; a rejected one is dropped there.
fn compile_on_parser_stack(
    source: &str,
    schemas: &PolicySchemaSnapshot,
) -> Result<Artifact, EngineConfigError> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("cel-parse".to_owned())
            .stack_size(PARSER_STACK_BYTES)
            .spawn_scoped(scope, || compile(source, schemas))
            .map_err(|_| check::config_error("parser thread could not be started"))?
            .join()
            .map_err(|_| check::config_error("expression could not be parsed within bounds"))?
    })
}

/// Parse `source` and type-check it under the schema of every metric type in
/// `schemas`, or with an untyped `request` when there is none.
fn compile(source: &str, schemas: &PolicySchemaSnapshot) -> Result<Artifact, EngineConfigError> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-parse
    let parsed = cel_core::parse(source);
    if let Some(error) = parsed.errors.first() {
        return Err(check::at(
            source,
            error.span.start,
            &format!("parse error: {}", error.message),
        ));
    }
    let expr = parsed
        .ast
        .ok_or_else(|| check::config_error("expression is empty"))?;
    // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-parse
    // @cpt-begin:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-static
    let requests = if schemas.schemas.is_empty() {
        vec![Shape::new(Kind::Dyn)]
    } else {
        schemas
            .schemas
            .values()
            .map(request_shape)
            .collect::<Result<_, _>>()?
    };
    let mut inputs = EnvironmentInputs::NONE;
    for request in requests {
        let mut checker = Checker {
            source,
            variables: environment_variables(request),
            nodes: 0,
            inputs: EnvironmentInputs::NONE,
        };
        checker.check_decision(&expr, 0)?;
        inputs.request |= checker.inputs.request;
    }
    let metrics = schemas.schemas.keys().cloned().collect();
    // @cpt-end:cpt-cf-quota-enforcement-algo-cel-engine:p1:inst-cel-static
    Ok(Artifact {
        source: Arc::from(source),
        expr,
        metrics,
        inputs,
    })
}

/// Bind the context into the runtime. Every bound value is measured against
/// the budget before the expression runs, so an oversized request cannot buy
/// itself evaluation time the budget did not grant.
fn bind_environment(
    runtime: &mut Runtime,
    context: &EvaluationContext<'_>,
) -> Result<(), EngineError> {
    let mut quotas = Vec::with_capacity(context.quotas.len());
    for quota in context.quotas {
        let snapshot = quota.snapshot;
        let mut value = Map::new();
        value.insert("id".into(), json!(snapshot.quota_id.to_string()));
        value.insert(
            "subject_type".into(),
            json!(snapshot.subject.subject_type.as_ref()),
        );
        value.insert("tier".into(), json!(tier_name(quota.tier)));
        value.insert("cap".into(), optional_int(snapshot.cap)?);
        value.insert("consumed".into(), json!(int(snapshot.consumed)?));
        value.insert(
            "remaining".into(),
            optional_int(
                snapshot
                    .cap
                    .map(|cap| cap.saturating_sub(snapshot.consumed)),
            )?,
        );
        let value = Value::Object(value);
        runtime::measure(&value)?;
        quotas.push(value);
    }
    let bindings = [
        ("request", context.request.clone()),
        ("amount", json!(int(context.amount)?)),
        ("metric_id", json!(context.metric_id.unwrap_or_default())),
        ("quotas", Value::Array(quotas)),
    ];
    for (name, value) in bindings {
        runtime.meter.charge(cost(&value)?)?;
        runtime.variables.insert(name.to_owned(), value);
    }
    Ok(())
}

/// A CEL `int` is signed 64-bit; a counter above `i64::MAX` cannot be
/// represented, and pretending otherwise would silently wrap.
fn int(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value)
        .map_err(|_| EngineError::TypeError("amount exceeds the CEL integer range".into()))
}

fn optional_int(value: Option<u64>) -> Result<Value, EngineError> {
    value.map_or(Ok(Value::Null), |v| int(v).map(Value::from))
}

fn tier_name(tier: QuotaScopeTier) -> &'static str {
    match tier {
        QuotaScopeTier::Subject => "subject",
        QuotaScopeTier::Tenant => "tenant",
    }
}

fn type_error(message: &str) -> EngineError {
    EngineError::TypeError(message.into())
}

/// Interpret the returned record as a [`Decision`]. Everything here is a
/// checked conversion: a negative or non-integer amount or an id that is not a
/// UUID is an engine type error, never a cast.
fn decode(record: Value, context: &EvaluationContext<'_>) -> Result<Decision, EngineError> {
    let Value::Object(mut record) = record else {
        return Err(type_error("expression must return a decision record"));
    };
    let plan = record.remove("debit_plan");
    let deny = record.remove("deny");
    if !record.is_empty() {
        return Err(type_error(
            "decision record allows only `debit_plan` or `deny`",
        ));
    }
    let (result, debit_plan) = match (plan, deny) {
        (Some(plan), None) => decode_plan(&plan)?,
        (None, Some(deny)) => (decode_deny(&deny)?, DebitPlan::new()),
        (Some(_), Some(_)) => {
            return Err(type_error(
                "decision record carries both `debit_plan` and `deny`",
            ));
        }
        (None, None) => return Err(type_error("decision record needs `debit_plan` or `deny`")),
    };
    Ok(Decision {
        result,
        diagnostics: diagnostics(context, &debit_plan),
        debit_plan,
    })
}

fn decode_plan(plan: &Value) -> Result<(DecisionResult, DebitPlan), EngineError> {
    let not_chains =
        || type_error("`debit_plan` must be a list of chains of `{id, amount}` records");
    let mut debit_plan = DebitPlan::new();
    for chain in plan.as_array().ok_or_else(not_chains)? {
        let mut entries = Vec::new();
        for entry in chain.as_array().ok_or_else(not_chains)? {
            let entry = entry.as_object().ok_or_else(not_chains)?;
            let quota_id = quota_id(entry.get("id"))?;
            let amount = entry
                .get("amount")
                .and_then(Value::as_i64)
                .ok_or_else(|| type_error("`debit_plan` amount must be an integer"))?;
            let amount =
                u64::try_from(amount).map_err(|_| type_error("`debit_plan` amount is negative"))?;
            entries.push(QuotaDebitPlan { quota_id, amount });
        }
        debit_plan.push(entries);
    }
    if debit_plan.iter().all(Vec::is_empty) {
        // A predicate that filtered out every Quota is a denial the operator
        // can act on, never an `Allowed` with nothing to debit.
        return Ok((
            DecisionResult::Denied {
                violated_quota_ids: Vec::new(),
                reason: NO_QUOTA_SELECTED.to_owned(),
            },
            DebitPlan::new(),
        ));
    }
    Ok((DecisionResult::Allowed, debit_plan))
}

fn decode_deny(deny: &Value) -> Result<DecisionResult, EngineError> {
    let deny = deny
        .as_object()
        .ok_or_else(|| type_error("`deny` must be a `{reason, violated_quota_ids}` record"))?;
    let reason = deny
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .ok_or_else(|| type_error("`deny` needs a non-empty `reason`"))?;
    let mut violated_quota_ids = Vec::new();
    if let Some(ids) = deny.get("violated_quota_ids") {
        for id in ids
            .as_array()
            .ok_or_else(|| type_error("`violated_quota_ids` must be a list"))?
        {
            violated_quota_ids.push(quota_id(Some(id))?);
        }
    }
    Ok(DecisionResult::Denied {
        violated_quota_ids,
        reason: reason.to_owned(),
    })
}

fn quota_id(value: Option<&Value>) -> Result<QuotaId, EngineError> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| type_error("quota id must be a string"))?;
    uuid::Uuid::parse_str(text)
        .map(QuotaId::new)
        .map_err(|_| type_error("quota id is not a UUID"))
}

/// The per-Quota detail the feature requires alongside the policy identity.
/// The boundary re-stamps `engine_id`, `policy_id` and `policy_version`; they
/// are set here too so a Decision read straight off the engine is complete.
fn diagnostics(context: &EvaluationContext<'_>, debit_plan: &DebitPlan) -> BTreeMap<String, Value> {
    let quotas: Map<String, Value> = context
        .quotas
        .iter()
        .map(|quota| {
            let snapshot = quota.snapshot;
            let contribution: u64 = debit_plan
                .iter()
                .flatten()
                .filter(|debit| debit.quota_id == snapshot.quota_id)
                .fold(0, |sum, debit| sum.saturating_add(debit.amount));
            (
                snapshot.quota_id.to_string(),
                json!({
                    "quota_id": snapshot.quota_id,
                    "quota_type": snapshot.quota_type,
                    "enforcement_mode": snapshot.enforcement_mode,
                    "tier": tier_name(quota.tier),
                    "consumed": snapshot.consumed,
                    "cap": snapshot.cap,
                    "requested": context.amount,
                    "contribution": contribution,
                }),
            )
        })
        .collect();
    BTreeMap::from([
        ("engine_id".to_owned(), json!(ENGINE_ID)),
        ("policy_id".to_owned(), json!(context.policy.policy_id)),
        ("policy_version".to_owned(), json!(context.policy.version)),
        ("quotas".to_owned(), Value::Object(quotas)),
    ])
}

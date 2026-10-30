#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;

use super::{BoolEvaluationError, EvaluationBudget};
use crate::backend::{CompiledDocument, EvaluationBackend, EvaluationContext, EvaluationError};
use crate::rego::RegoBackend;

/// Work that no test can finish: 10^10 iterations over two small ranges.
const UNBOUNDED: &str = "package p\n\nrule := count([1 | some a in numbers.range(1, 100000); some b in numbers.range(1, 100000)])\n";

fn compile(source: &str) -> Arc<dyn CompiledDocument> {
    RegoBackend::new()
        .compile("test.rego", source, "rule")
        .unwrap()
}

fn ctx() -> EvaluationContext {
    EvaluationContext::new(json!({"action": "create"}), OffsetDateTime::now_utc()).unwrap()
}

fn generous() -> EvaluationBudget {
    EvaluationBudget::new(Duration::from_secs(5))
}

#[test]
fn booleans_and_undefined_map_to_a_flag() {
    let mut budget = generous();
    let cases = [
        ("package p\n\nrule := true\n", true),
        ("package p\n\nrule := false\n", false),
        ("package p\n\nrule if input.action == \"delete\"\n", false),
    ];
    for (source, expected) in cases {
        assert_eq!(
            budget.evaluate_bool(compile(source).as_ref(), &ctx()),
            Ok(expected),
            "{source}"
        );
    }
}

#[test]
fn any_other_value_is_an_error_never_false() {
    let doc = compile("package p\n\nrule := \"yes\"\n");
    assert_eq!(
        generous().evaluate_bool(doc.as_ref(), &ctx()),
        Err(BoolEvaluationError::NotBoolean)
    );
}

#[test]
fn an_explicit_null_is_an_error_never_false() {
    let doc = compile("package p\n\nrule := null\n");
    assert_eq!(
        generous().evaluate_bool(doc.as_ref(), &ctx()),
        Err(BoolEvaluationError::NotBoolean)
    );
}

#[test]
fn a_backend_failure_keeps_its_detail() {
    let doc = compile("package p\n\nrule := to_number(\"not a number\")\n");
    let err = generous().evaluate_bool(doc.as_ref(), &ctx()).unwrap_err();
    assert!(
        matches!(
            &err,
            BoolEvaluationError::Evaluation(EvaluationError::Failed { message }) if !message.is_empty()
        ),
        "{err:?}"
    );
}

#[test]
fn an_exhausted_budget_evaluates_nothing() {
    let mut budget = EvaluationBudget::new(Duration::ZERO);
    assert!(budget.is_exhausted());
    assert_eq!(
        budget.evaluate(compile("package p\n\nrule := true\n").as_ref(), &ctx()),
        Err(EvaluationError::BoundExceeded {
            limit: Duration::ZERO
        })
    );
}

#[test]
fn an_evaluation_past_what_remains_exceeds_the_budget() {
    let limit = Duration::from_millis(5);
    let mut budget = EvaluationBudget::new(limit);
    assert_eq!(
        budget.evaluate(compile(UNBOUNDED).as_ref(), &ctx()),
        Err(EvaluationError::BoundExceeded { limit })
    );
    assert!(budget.is_exhausted());
}

#[test]
fn each_evaluation_is_charged_and_the_rest_carries_over() {
    let mut budget = generous();
    let doc = compile("package p\n\nrule := true\n");
    budget.evaluate(doc.as_ref(), &ctx()).unwrap();
    let after_one = budget.remaining();
    assert!(after_one < budget.limit());
    budget.evaluate(doc.as_ref(), &ctx()).unwrap();
    assert!(budget.remaining() < after_one);
    assert_eq!(budget.limit(), Duration::from_secs(5));
}

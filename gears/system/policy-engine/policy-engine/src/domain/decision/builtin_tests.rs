//! Built-in selection and first-denial evaluation over the real Rego backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use gts::GtsId;
use time::OffsetDateTime;
use toolkit_policy_evaluation::EvaluationBudget;
use uuid::Uuid;

use super::BuiltinPolicySet;
use crate::domain::decision::compiled::DocumentCompileError;
use crate::domain::decision::test_support::{
    DENY_ALL, DENY_DELETE, DENY_NONE, GADGET, NOT_BOOLEAN, WIDGET, document,
};
use crate::domain::evaluator::{EvaluatorFailure, RequestFacts};
use crate::domain::model::Document;

fn set(documents: &[Document]) -> BuiltinPolicySet {
    BuiltinPolicySet::compile(documents).unwrap()
}

fn facts(action: &str) -> RequestFacts {
    RequestFacts {
        action: action.to_owned(),
        resource_type: WIDGET.to_owned(),
        resource_id: None,
        resource_tenant_id: Uuid::from_u128(1),
        properties: serde_json::Map::new(),
        subject_id: Uuid::from_u128(2),
        subject_tenant_id: Uuid::from_u128(1),
    }
}

fn widget() -> GtsId {
    GtsId::try_new(WIDGET).unwrap()
}

fn first_denial(set: &BuiltinPolicySet, action: &str) -> Result<Option<String>, EvaluatorFailure> {
    set.first_denial(
        &widget(),
        &facts(action),
        OffsetDateTime::now_utc(),
        &mut EvaluationBudget::new(Duration::from_secs(5)),
    )
}

#[test]
fn selection_follows_resource_types_and_actions() {
    let set = set(&[document("widget-delete", DENY_ALL, &[WIDGET], &["delete"])]);
    assert!(set.applies_to(&widget(), "delete"));
    assert!(!set.applies_to(&widget(), "create"));
    assert!(!set.applies_to(&GtsId::try_new(GADGET).unwrap(), "delete"));
    assert!(!BuiltinPolicySet::default().applies_to(&widget(), "delete"));
}

#[test]
fn the_first_denial_in_configuration_order_wins_and_ends_evaluation() {
    // `broken` would fail evaluation if it ran after the denial.
    let set = set(&[
        document("silent", DENY_NONE, &[WIDGET], &[]),
        document("deletes", DENY_DELETE, &[WIDGET], &[]),
        document("everything", DENY_ALL, &[WIDGET], &[]),
        document("broken", NOT_BOOLEAN, &[WIDGET], &[]),
    ]);
    assert_eq!(
        first_denial(&set, "delete").unwrap().as_deref(),
        Some("deletes")
    );
    assert_eq!(
        first_denial(&set, "create").unwrap().as_deref(),
        Some("everything")
    );
}

#[test]
fn policies_that_do_not_apply_are_not_evaluated() {
    let set = set(&[
        document("gadgets", NOT_BOOLEAN, &[GADGET], &[]),
        document("creates", NOT_BOOLEAN, &[WIDGET], &["create"]),
        document("silent", DENY_NONE, &[WIDGET], &[]),
    ]);
    assert_eq!(first_denial(&set, "delete").unwrap(), None);
}

#[test]
fn a_failing_policy_before_any_denial_fails_the_evaluation() {
    let set = set(&[
        document("broken", NOT_BOOLEAN, &[WIDGET], &[]),
        document("everything", DENY_ALL, &[WIDGET], &[]),
    ]);
    assert_eq!(
        first_denial(&set, "create").unwrap_err(),
        EvaluatorFailure::Unmappable {
            document: "broken".to_owned()
        }
    );
}

#[test]
fn an_exhausted_budget_is_a_timeout_never_a_permit() {
    let set = set(&[document("everything", DENY_ALL, &[WIDGET], &[])]);
    let result = set.first_denial(
        &widget(),
        &facts("create"),
        OffsetDateTime::now_utc(),
        &mut EvaluationBudget::new(Duration::ZERO),
    );
    assert_eq!(
        result.unwrap_err(),
        EvaluatorFailure::Timeout {
            document: "everything".to_owned()
        }
    );
}

#[test]
fn content_that_does_not_compile_is_rejected() {
    let err = BuiltinPolicySet::compile(&[document("bad", "package p\ndeny :=", &[WIDGET], &[])])
        .unwrap_err();
    assert!(err.to_string().contains("`bad`"), "{err}");
}

#[test]
fn content_using_a_denylisted_builtin_is_rejected() {
    let err = BuiltinPolicySet::compile(&[document(
        "clock",
        "package p\ndeny if time.now_ns() > 0",
        &[WIDGET],
        &[],
    )])
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "document `clock`: references denylisted builtin(s): time.now_ns"
    );
}

#[test]
fn an_invalid_resource_type_pattern_is_rejected() {
    let err = BuiltinPolicySet::compile(&[document("odd", DENY_ALL, &["not a gts id"], &[])])
        .unwrap_err();
    assert!(
        matches!(&err, DocumentCompileError::Pattern { document, pattern, .. }
            if document == "odd" && pattern == "not a gts id"),
        "{err}"
    );
}

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use admission_control_sdk::EngineResult;
use toolkit_policy_evaluation::{EvaluationBackend, RegoBackend};

use super::*;

fn facts() -> RequestFacts {
    RequestFacts {
        action: "create".to_owned(),
        resource_type: crate::test_support::WIDGET.to_owned(),
        resource_id: None,
        resource_tenant_id: Uuid::from_u128(2),
        properties: Map::new(),
        subject_id: Uuid::from_u128(3),
        subject_tenant_id: Uuid::from_u128(4),
    }
}

fn doc(n: u128, source: &str, enforce: bool) -> ApplicableDocument {
    ApplicableDocument {
        compiled: RegoBackend::new()
            .compile("doc", source, "deny")
            .expect("compiles"),
        reference: PolicyReference {
            bundle_id: Uuid::from_u128(200),
            version_id: Uuid::from_u128(100),
            document_id: Uuid::from_u128(n),
            document_name: format!("doc{n}"),
        },
        enforce,
    }
}

fn run(docs: &[ApplicableDocument], budget: Duration) -> Result<EngineResult, EvaluatorFailure> {
    evaluate_documents(
        docs,
        &facts(),
        OffsetDateTime::now_utc(),
        &mut EvaluationBudget::new(budget),
    )
    .map(|result| result.finish("POLICY_DENIED"))
}

fn names(references: &[PolicyReference]) -> Vec<&str> {
    references
        .iter()
        .map(|r| r.document_name.as_str())
        .collect()
}

#[test]
fn every_document_is_evaluated_against_the_request_input() {
    let docs = [
        doc(
            1,
            "package p\ndeny := true if input.action == \"create\"",
            true,
        ),
        doc(
            2,
            "package p\ndeny := true if input.action == \"delete\"",
            true,
        ),
        doc(
            3,
            "package p\ndeny := true if input.subject.tenant_id == \"00000000-0000-0000-0000-000000000004\"",
            true,
        ),
        doc(
            4,
            "package p\ndeny := true if input.resource.tenant_id == \"00000000-0000-0000-0000-000000000002\"",
            true,
        ),
    ];
    let EngineResult::Deny { denials, .. } = run(&docs, Duration::from_secs(5)).unwrap() else {
        panic!("expected a denial");
    };
    assert_eq!(names(&denials), ["doc1", "doc3", "doc4"]);
}

#[test]
fn non_enforcing_denials_are_shadow_denials() {
    let docs = [
        doc(1, "package p\ndeny := true", false),
        doc(2, "package p\ndeny := false", true),
    ];
    let EngineResult::Permit { shadow_denials } = run(&docs, Duration::from_secs(5)).unwrap()
    else {
        panic!("a shadow denial alone permits");
    };
    assert_eq!(names(&shadow_denials), ["doc1"]);
}

#[test]
fn a_non_boolean_deny_is_an_error_not_a_permit() {
    let docs = [doc(1, "package p\ndeny := \"yes\"", true)];
    assert_eq!(
        run(&docs, Duration::from_secs(5)).unwrap_err(),
        EvaluatorFailure::Unmappable {
            document: "doc1".to_owned()
        }
    );
}

#[test]
fn a_backend_failure_keeps_its_cause() {
    let docs = [doc(1, "package p\ndeny := to_number(\"nan?\") > 0", true)];
    let err = run(&docs, Duration::from_secs(5)).unwrap_err();
    assert!(
        matches!(&err, EvaluatorFailure::Backend { document, message }
            if document == "doc1" && !message.is_empty()),
        "{err:?}"
    );
}

#[test]
fn an_exhausted_budget_fails_the_evaluation() {
    let docs = [doc(1, "package p\ndeny := true", true)];
    assert_eq!(
        run(&docs, Duration::ZERO).unwrap_err(),
        EvaluatorFailure::Timeout {
            document: "doc1".to_owned()
        }
    );
}

#[test]
fn an_evaluation_past_the_budget_is_a_timeout() {
    let docs = [doc(
        1,
        "package p\ndeny := count([1 | some a in numbers.range(1, 100000); some b in numbers.range(1, 100000)]) > 0",
        true,
    )];
    assert_eq!(
        run(&docs, Duration::from_millis(5)).unwrap_err(),
        EvaluatorFailure::Timeout {
            document: "doc1".to_owned()
        }
    );
}

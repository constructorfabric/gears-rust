#![allow(clippy::unwrap_used, clippy::expect_used)]

use toolkit_policy_evaluation::{EvaluationBackend, RegoBackend};

use super::*;
use crate::domain::model::{BundleId, DocumentId, VersionId};

fn facts() -> RequestFacts {
    RequestFacts {
        action: "create".to_owned(),
        resource_type: "gts.cf.core.example.widget.v1~".to_owned(),
        resource_id: None,
        resource_tenant_id: Uuid::from_u128(2),
        properties: Map::new(),
        subject_id: Uuid::from_u128(3),
        subject_tenant_id: Uuid::from_u128(4),
    }
}

fn doc(n: u128, source: &str) -> ApplicableDocument {
    ApplicableDocument {
        compiled: RegoBackend::new()
            .compile("doc", source, "deny")
            .expect("compiles"),
        key: EvaluatedDocumentKey {
            document_id: DocumentId(Uuid::from_u128(n)),
            document_name: format!("doc{n}"),
            version_id: VersionId(Uuid::from_u128(100)),
            bundle_id: BundleId(Uuid::from_u128(200)),
        },
        enforce: true,
    }
}

fn run(docs: &[ApplicableDocument]) -> Result<Vec<bool>, EvaluatorFailure> {
    evaluate_documents(
        docs,
        &facts(),
        OffsetDateTime::now_utc(),
        Duration::from_secs(5),
    )
    .map(|all| all.iter().map(|d| d.denied).collect())
}

#[test]
fn every_document_is_evaluated_against_the_request_input() {
    let docs = [
        doc(1, "package p\ndeny := true if input.action == \"create\""),
        doc(2, "package p\ndeny := true if input.action == \"delete\""),
        doc(
            3,
            "package p\ndeny := true if input.subject.tenant_id == \"00000000-0000-0000-0000-000000000004\"",
        ),
        doc(
            4,
            "package p\ndeny := true if input.resource.tenant_id == \"00000000-0000-0000-0000-000000000002\"",
        ),
    ];
    assert_eq!(run(&docs).unwrap(), [true, false, true, true]);
}

#[test]
fn a_non_boolean_deny_is_an_error_not_a_permit() {
    let docs = [doc(1, "package p\ndeny := \"yes\"")];
    assert!(matches!(
        run(&docs),
        Err(EvaluatorFailure::Unmappable { .. })
    ));
}

#[test]
fn an_exhausted_budget_fails_the_evaluation() {
    let docs = [doc(1, "package p\ndeny := true")];
    let result = evaluate_documents(&docs, &facts(), OffsetDateTime::now_utc(), Duration::ZERO);
    assert!(matches!(result, Err(EvaluatorFailure::Timeout { .. })));
}

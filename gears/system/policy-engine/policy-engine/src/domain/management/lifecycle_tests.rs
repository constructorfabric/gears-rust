#![allow(clippy::unwrap_used, clippy::expect_used)]

use policy_engine_sdk::management::{self as sdk, DocumentSpec, VersionContent, finding, reason};
use uuid::Uuid;

use super::*;
use crate::domain::management::error::reason_of;
use crate::domain::management::test_support::{TEST_PATTERN, WIDGET, deny_on, draft, limits};
use crate::domain::model::{BundleId, VersionState};

fn spec(name: &str) -> DocumentSpec {
    DocumentSpec {
        name: name.to_owned(),
        content: deny_on(name, "delete"),
        resource_types: vec![WIDGET.to_owned(), TEST_PATTERN.to_owned()],
        actions: vec!["delete".to_owned()],
    }
}

fn content(names: &[&str]) -> VersionContent {
    VersionContent {
        documents: names.iter().map(|n| spec(n)).collect(),
    }
}

#[test]
fn sdk_content_round_trips_through_the_domain() {
    let written = content(&["b_doc", "a_doc"]);
    let mut documents = documents_from_content(&written);
    // Identities are fresh per call.
    assert_ne!(documents[0].id, documents_from_content(&written)[0].id);
    documents.sort_by(|a, b| a.name.cmp(&b.name));
    let version = BundleVersion {
        documents,
        ..draft(BundleId(Uuid::new_v4()), Uuid::new_v4(), Vec::new())
    };
    let detail = detail_to_sdk(&version);
    let mut expected: Vec<DocumentSpec> = written.documents;
    expected.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        detail
            .documents
            .iter()
            .map(|d| d.spec.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(detail.version.state, sdk::VersionState::Draft);
    assert_eq!(detail.version.ordinal, 1);
}

#[test]
fn version_header_carries_the_state() {
    let mut version = draft(BundleId(Uuid::new_v4()), Uuid::new_v4(), Vec::new());
    version.state = VersionState::Superseded;
    assert_eq!(
        version_to_sdk(&version).state,
        sdk::VersionState::Superseded
    );
    version.state = VersionState::Active;
    assert_eq!(version_to_sdk(&version).state, sdk::VersionState::Active);
}

#[test]
fn write_checks_refuse_what_the_store_cannot_hold() {
    let ok = documents_from_content(&content(&["a"]));
    assert!(check_write(&limits(), &ok).is_ok());

    let too_many: Vec<&str> = vec!["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"];
    let err = check_write(&limits(), &documents_from_content(&content(&too_many))).unwrap_err();
    assert_eq!(
        reason_of(&err).as_deref(),
        Some(reason::CONTENT_LIMIT_EXCEEDED)
    );

    let err = check_write(&limits(), &documents_from_content(&content(&["a", "a"]))).unwrap_err();
    assert_eq!(
        reason_of(&err).as_deref(),
        Some(finding::DUPLICATE_DOCUMENT_NAME)
    );
}

#[test]
fn seeding_copies_content_under_fresh_identities() {
    let documents = documents_from_content(&content(&["a", "b"]));
    let seed = draft(BundleId(Uuid::new_v4()), Uuid::new_v4(), documents);
    let copy = seed_documents(&seed);
    assert_eq!(copy.len(), 2);
    for (original, copied) in seed.documents.iter().zip(&copy) {
        assert_ne!(original.id, copied.id);
        assert_eq!(original.content, copied.content);
        assert_eq!(original.resource_types, copied.resource_types);
        assert_eq!(original.actions, copied.actions);
    }
}

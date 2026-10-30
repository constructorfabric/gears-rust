use admission_control_sdk::{IDENTIFIER_MAX_LEN, MAX_DOCUMENT_NAME_LEN, RESOURCE_TYPE_MAX_LEN};

use super::*;

fn doc(name: &str, content: &str) -> Document {
    Document {
        id: DocumentId(Uuid::new_v4()),
        name: name.to_owned(),
        content: content.to_owned(),
        resource_types: Vec::new(),
        actions: Vec::new(),
    }
}

#[test]
fn version_state_codes_round_trip() {
    for (state, code) in [
        (VersionState::Draft, 1),
        (VersionState::Active, 2),
        (VersionState::Superseded, 3),
    ] {
        assert_eq!(state.code(), code);
        assert_eq!(VersionState::from_code(code), Some(state));
    }
    assert_eq!(VersionState::from_code(0), None);
    assert_eq!(VersionState::from_code(4), None);
}

#[test]
fn limits_report_version_then_document_violations() {
    let limits = ContentLimits {
        max_documents_per_version: 1,
        max_document_bytes: 3,
    };
    assert!(limits.check(&[doc("a", "abc")]).is_empty());
    let kinds: Vec<_> = limits
        .check(&[doc("a", "abcd"), doc("b", "abc")])
        .into_iter()
        .map(|v| (v.kind, v.document))
        .collect();
    assert_eq!(
        kinds,
        [
            (LimitKind::DocumentsPerVersion, None),
            (LimitKind::DocumentBytes, Some("a".to_owned())),
        ]
    );
}

#[test]
fn each_document_bound_is_reported() {
    let limits = ContentLimits {
        max_documents_per_version: 8,
        max_document_bytes: 4,
    };
    let document = Document {
        resource_types: vec![
            "t".repeat(RESOURCE_TYPE_MAX_LEN + 1);
            limits::MAX_RESOURCE_TYPES_PER_DOCUMENT + 1
        ],
        actions: vec!["a".repeat(IDENTIFIER_MAX_LEN + 1); limits::MAX_ACTIONS_PER_DOCUMENT + 1],
        ..doc(&"n".repeat(MAX_DOCUMENT_NAME_LEN + 1), "12345")
    };
    let kinds: Vec<_> = limits
        .check_document(&document)
        .into_iter()
        .map(|v| v.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            LimitKind::DocumentBytes,
            LimitKind::DocumentNameBytes,
            LimitKind::ResourceTypesPerDocument,
            LimitKind::ActionsPerDocument,
            LimitKind::ResourceTypeBytes,
            LimitKind::ActionBytes,
        ]
    );
    assert!(limits.check_document(&doc("ok", "1234")).is_empty());
}

#[test]
fn a_document_name_must_be_printable_and_not_blank() {
    assert!(document_name_valid("deny-deletes"));
    for name in ["", "  ", "a\nb", "tab\there"] {
        assert!(!document_name_valid(name), "{name:?}");
    }
}

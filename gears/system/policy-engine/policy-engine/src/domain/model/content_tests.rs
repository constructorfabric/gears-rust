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

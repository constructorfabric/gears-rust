use uuid::Uuid;

use super::*;

fn doc(n: u128, enforce: bool, denied: bool) -> EvaluatedDocument {
    EvaluatedDocument {
        key: EvaluatedDocumentKey {
            document_id: DocumentId(Uuid::from_u128(n)),
            document_name: format!("doc{n}"),
            version_id: VersionId(Uuid::from_u128(200 + n)),
            bundle_id: BundleId(Uuid::from_u128(300 + n)),
        },
        enforce,
        denied,
    }
}

#[test]
fn nothing_denying_permits() {
    assert!(!combine(vec![doc(1, true, false), doc(2, false, false)]).is_denied());
    assert_eq!(combine(Vec::new()), CombinedResult::default());
}

#[test]
fn enforced_denials_deny_and_shadow_denials_do_not() {
    let result = combine(vec![
        doc(1, false, true),
        doc(2, true, true),
        doc(3, true, true),
        doc(4, true, false),
    ]);
    assert!(result.is_denied());
    assert_eq!(
        result.denials,
        vec![doc(2, true, true).key, doc(3, true, true).key]
    );
    assert_eq!(result.shadow_denials, vec![doc(1, false, true).key]);

    let shadow_only = combine(vec![doc(1, false, true)]);
    assert!(!shadow_only.is_denied());
    assert_eq!(shadow_only.shadow_denials.len(), 1);
}

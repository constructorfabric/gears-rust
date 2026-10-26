//! The profile root's payload and the version counter.

use serde_json::json;
use uuid::Uuid;

use super::root::{PayloadCounter, ProfileVersion, Root, root_key};
use crate::domain::error::DomainError;

#[test]
fn the_root_key_names_tenant_and_subject() {
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    assert_eq!(
        root_key(tenant, subject),
        format!("construct:{tenant}:root:{subject}")
    );
    assert_ne!(root_key(tenant, subject), root_key(Uuid::new_v4(), subject));
}

#[test]
fn a_root_payload_reads_back_as_written() {
    let subject = Uuid::new_v4();
    let root = Root {
        version: 4,
        removals: vec!["construct:t:a".to_owned()],
    };

    let payload = root.payload(subject);

    assert_eq!(payload["subject_id"], json!(subject.to_string()));
    assert_eq!(Root::from_payload(Some(&payload)).unwrap(), root);
}

#[test]
fn a_root_without_a_valid_payload_is_a_defect() {
    for payload in [
        None,
        Some(json!({ "removals": [] })),
        Some(json!({ "version": 1 })),
        Some(json!({ "version": 1, "removals": [3] })),
    ] {
        let result = Root::from_payload(payload.as_ref());
        assert!(
            matches!(result, Err(DomainError::Internal(_))),
            "{payload:?}: {result:?}"
        );
    }
}

#[test]
fn a_new_subject_creates_its_root_at_version_one_and_every_write_adds_one() {
    let counter = PayloadCounter;

    assert_eq!(counter.expected(None), 0, "claims a key no node holds");
    assert_eq!(counter.next(None), 1);
    assert_eq!(counter.expected(Some(3)), 3);
    assert_eq!(counter.next(Some(3)), 4);
    let stored = json!({ "version": 3, "removals": [] });
    assert_eq!(counter.read(Some(&stored)).unwrap(), 3);
}

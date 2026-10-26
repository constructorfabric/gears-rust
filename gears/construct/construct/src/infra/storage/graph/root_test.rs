//! The profile root's payload and the version counter.

use serde_json::{Value, json};
use uuid::Uuid;

use construct_sdk::gts::PROFILE_ROOT_TYPE;

use super::fact_test::view;
use super::root::{PayloadCounter, ProfileVersion, Root, root_key};
use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::subject_settings::SubjectKey;

fn key() -> NodeKey {
    NodeKey("construct:t:root:s".to_owned())
}

fn internal_message(result: Result<impl std::fmt::Debug, DomainError>) -> String {
    match result {
        Err(DomainError::Internal(message)) => message,
        other => panic!("an internal error, got {other:?}"),
    }
}

#[test]
fn the_root_key_names_tenant_and_subject() {
    let subject = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };

    assert_eq!(
        root_key(&subject).0,
        format!(
            "construct:{}:root:{}",
            subject.tenant_id, subject.subject_id
        )
    );
}

#[test]
fn a_root_payload_reads_back_as_written_with_the_counter_stamped() {
    let subject = Uuid::new_v4();
    let root = Root {
        removals: vec![NodeKey("construct:t:a".to_owned())],
    };

    let payload = root.payload(subject, &PayloadCounter, 4).unwrap();

    assert_eq!(
        payload,
        json!({ "subject_id": subject.to_string(), "version": 4, "removals": ["construct:t:a"] })
    );
    assert_eq!(
        Root::from_payload(&key(), subject, Some(&payload)).unwrap(),
        root
    );
}

#[test]
fn a_root_that_names_another_subject_is_a_defect() {
    let payload = Root::default()
        .payload(Uuid::new_v4(), &PayloadCounter, 1)
        .unwrap();

    let message = internal_message(Root::from_payload(&key(), Uuid::new_v4(), Some(&payload)));

    assert!(message.contains("construct:t:root:s"), "{message}");
    assert!(message.contains("names subject"), "{message}");
}

#[test]
fn a_root_without_a_valid_payload_names_the_root_and_what_is_wrong() {
    let no_payload = internal_message(Root::from_payload(&key(), Uuid::new_v4(), None));
    assert!(no_payload.contains("construct:t:root:s"), "{no_payload}");

    let broken: Value = json!({ "subject_id": Uuid::new_v4().to_string(), "removals": [3] });
    let message = internal_message(Root::from_payload(&key(), Uuid::new_v4(), Some(&broken)));
    assert!(message.contains("construct:t:root:s"), "{message}");
    assert!(
        message.contains("invalid type"),
        "names the field error: {message}"
    );
}

#[test]
fn a_new_subject_creates_its_root_at_version_one_and_every_write_adds_one() {
    let counter = PayloadCounter;

    assert_eq!(
        counter.expected(None).unwrap(),
        0,
        "claims a key no node holds"
    );
    assert_eq!(counter.next(None).unwrap(), 1);
    assert_eq!(counter.expected(Some(3)).unwrap(), 3);
    assert_eq!(counter.next(Some(3)).unwrap(), 4);
    let max = u64::try_from(i64::MAX).unwrap();
    assert_eq!(counter.expected(Some(max)).unwrap(), i64::MAX);
}

#[test]
fn a_version_beyond_graph_storages_range_is_a_defect_not_a_conflict() {
    let counter = PayloadCounter;
    let beyond = u64::try_from(i64::MAX).unwrap() + 1;

    let message = internal_message(counter.expected(Some(beyond)));

    assert!(message.contains(&beyond.to_string()), "{message}");
    internal_message(counter.next(Some(u64::MAX)));
}

#[test]
fn the_counter_reads_the_version_a_root_carries() {
    let root = view(
        PROFILE_ROOT_TYPE,
        Some(json!({ "subject_id": Uuid::new_v4().to_string(), "version": 7, "removals": [] })),
    );

    assert_eq!(PayloadCounter.read(&root).unwrap(), 7);
}

#[test]
fn a_root_without_a_readable_version_is_a_defect() {
    for payload in [
        None,
        Some(json!({ "removals": [] })),
        Some(json!({ "version": -1 })),
        Some(json!({ "version": "1" })),
        Some(json!({ "version": 1.5 })),
    ] {
        let root = view(PROFILE_ROOT_TYPE, payload.clone());

        let message = internal_message(PayloadCounter.read(&root));

        assert!(
            message.contains("construct:t:fact"),
            "{payload:?}: {message}"
        );
    }
}

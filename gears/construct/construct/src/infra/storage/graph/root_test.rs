//! The profile root's payload and the version counter.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use uuid::Uuid;

use construct_sdk::gts::PROFILE_ROOT_TYPE;

use super::fact_test::view;
use super::root::{
    PayloadCounter, ProfileVersion, Root, generation_root, root_generation, root_key,
};
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
fn a_root_key_reads_back_as_the_generation_it_was_built_for() {
    let subject = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };
    for generation in [0, 1, 7, u64::MAX] {
        assert_eq!(
            root_generation(&subject, &generation_root(&subject, generation)),
            Some(generation)
        );
    }

    let first = root_key(&subject).0;
    let other_tenant = SubjectKey {
        tenant_id: Uuid::new_v4(),
        ..subject
    };
    let other_subject = SubjectKey {
        subject_id: Uuid::new_v4(),
        ..subject
    };
    for foreign in [
        generation_root(&other_tenant, 0).0,
        generation_root(&other_subject, 3).0,
        format!("{first}:x"),
        format!("{first}3"),
        format!("{first}:0"),
        format!("{first}:03"),
        format!("{first}:+3"),
        format!("{first}:3 "),
        format!("{first}:"),
    ] {
        assert_eq!(
            root_generation(&subject, &NodeKey(foreign.clone())),
            None,
            "{foreign}"
        );
    }
}

#[test]
fn a_root_payload_reads_back_as_written_with_the_counter_stamped() {
    let subject = Uuid::new_v4();
    let root = Root {
        removals: BTreeSet::from([NodeKey("construct:t:a".to_owned())]),
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
}

/// The version a root of `generation` with `counter` reads as.
fn version_of(generation: u64, counter: u64) -> u64 {
    let subject = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };
    let mut root = view(
        PROFILE_ROOT_TYPE,
        Some(
            json!({ "subject_id": subject.subject_id.to_string(), "version": counter, "removals": [] }),
        ),
    );
    root.node_key = generation_root(&subject, generation).0;
    PayloadCounter.read(&root).unwrap()
}

#[test]
fn a_root_of_a_later_generation_never_reads_a_version_an_earlier_one_had() {
    assert_eq!(
        version_of(0, 1),
        1,
        "generation 0 reads its counter, as before"
    );
    assert_ne!(version_of(1, 1), version_of(0, 1));
    assert_ne!(version_of(2, 1), version_of(1, 1));
    assert!(version_of(1, 1) > version_of(0, u64::from(u32::MAX)));
}

#[test]
fn graph_storage_sees_only_the_counter_and_a_write_advances_it_in_its_generation() {
    let counter = PayloadCounter;
    let version = version_of(3, 5);

    assert_eq!(counter.expected(Some(version)).unwrap(), 5);
    let next = counter.next(Some(version)).unwrap();
    assert_eq!(next, version_of(3, 6));
    let mut payload = serde_json::Map::new();
    counter.stamp(&mut payload, next);
    assert_eq!(payload["version"], json!(6));
}

#[test]
fn a_counter_at_its_limit_cannot_advance_and_one_beyond_it_is_a_defect() {
    let counter = PayloadCounter;
    let last = version_of(1, u64::from(u32::MAX));

    let message = internal_message(counter.next(Some(last)));
    assert!(message.contains(&last.to_string()), "{message}");
    let beyond = view(
        PROFILE_ROOT_TYPE,
        Some(
            json!({ "subject_id": Uuid::new_v4().to_string(), "version": u64::from(u32::MAX) + 1, "removals": [] }),
        ),
    );
    internal_message(PayloadCounter.read(&beyond));
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

#[test]
fn a_root_names_its_subject_and_an_unreadable_one_says_why() {
    let subject_id = uuid::Uuid::new_v4();
    let key = NodeKey("construct:t:root:s".to_owned());
    let payload = serde_json::json!({ "subject_id": subject_id, "removals": [] });
    assert_eq!(Root::subject_of(&key, Some(&payload)).unwrap(), subject_id);

    let err = Root::subject_of(&key, Some(&serde_json::json!({ "subject_id": "nope" })))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no valid payload"), "{err}");
    assert!(err.contains("construct:t:root:s"), "{err}");
    let err = Root::subject_of(&key, None).unwrap_err().to_string();
    assert!(err.contains("has no payload"), "{err}");
}

use super::{UserEvent, Uuid};
use crate::domain::events::UserDomainEvent;
use time::OffsetDateTime;

#[test]
fn maps_domain_event_to_transport() {
    let at = OffsetDateTime::from_unix_timestamp(1_699_963_200).unwrap();
    let id = Uuid::nil();
    let de = UserDomainEvent::Created { id, at };
    let out = UserEvent::from(&de);
    assert_eq!(out.kind, "created");
    assert_eq!(out.id, id);
    assert_eq!(out.at, at);
}

#[test]
fn maps_all_domain_event_variants() {
    let at = OffsetDateTime::from_unix_timestamp(1_699_963_200).unwrap();
    let id = Uuid::nil();

    // Test Created event
    let created = UserDomainEvent::Created { id, at };
    let created_event = UserEvent::from(&created);
    assert_eq!(created_event.kind, "created");
    assert_eq!(created_event.id, id);
    assert_eq!(created_event.at, at);

    // Test Updated event
    let updated = UserDomainEvent::Updated { id, at };
    let updated_event = UserEvent::from(&updated);
    assert_eq!(updated_event.kind, "updated");
    assert_eq!(updated_event.id, id);
    assert_eq!(updated_event.at, at);

    // Test Deleted event
    let deleted = UserDomainEvent::Deleted { id, at };
    let deleted_event = UserEvent::from(&deleted);
    assert_eq!(deleted_event.kind, "deleted");
    assert_eq!(deleted_event.id, id);
    assert_eq!(deleted_event.at, at);
}

#[test]
fn serializes_event_timestamp_with_millis() {
    let input = serde_json::json!({
        "kind": "created",
        "id": Uuid::nil(),
        "at": "2023-11-14T12:00:00.123Z"
    });

    let ev: UserEvent = serde_json::from_value(input).unwrap();
    assert_eq!(ev.at.unix_timestamp(), 1_699_963_200);
    assert_eq!(ev.at.nanosecond(), 123_000_000);
}

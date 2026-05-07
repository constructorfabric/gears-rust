use chrono::Utc;
use uuid::Uuid;

use super::{SubjectRef, UsageKind, UsageRecord};

fn make_record() -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id: Uuid::nil(),
        metric: "test.metric".to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::nil(),
        resource_type: "test.resource".to_owned(),
        subject: Some(SubjectRef {
            id: Uuid::nil(),
            kind: "test.subject".to_owned(),
        }),
        idempotency_key: Uuid::new_v4(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

#[test]
fn usage_record_roundtrip_serde() {
    let mut rec = make_record();
    rec.value = 42.0;
    let json = serde_json::to_string(&rec).unwrap();
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    assert!((deserialized.value - 42.0_f64).abs() < f64::EPSILON);
    assert_eq!(deserialized.kind, UsageKind::Gauge);
    assert_eq!(deserialized.idempotency_key, rec.idempotency_key);
}

#[test]
fn usage_record_clone_copies_all_fields() {
    let rec = make_record();
    let cloned = rec.clone();
    assert_eq!(cloned.tenant_id, rec.tenant_id);
    assert!((cloned.value - rec.value).abs() < f64::EPSILON);
    assert_eq!(cloned.resource_id, rec.resource_id);
    assert_eq!(cloned.subject, rec.subject);
    assert_eq!(cloned.idempotency_key, rec.idempotency_key);
}

#[test]
fn usage_record_subject_none_serde() {
    let rec = UsageRecord {
        subject: None,
        ..make_record()
    };
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        !json.contains("\"subject\""),
        "subject must be absent from JSON when None, got: {json}"
    );
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.subject, None);
}

#[test]
fn subject_ref_both_fields_present_after_roundtrip() {
    let rec = make_record();
    let json = serde_json::to_string(&rec).unwrap();
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    let subject = deserialized.subject.expect("subject should be present");
    assert_eq!(subject.id, Uuid::nil());
    assert_eq!(subject.kind, "test.subject");
}

#[test]
fn usage_kind_is_hashable() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(UsageKind::Gauge);
    set.insert(UsageKind::Counter);
    set.insert(UsageKind::Gauge);
    assert_eq!(set.len(), 2);
}

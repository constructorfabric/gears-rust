//! Unit tests for the error model.
#![allow(unknown_lints, de0901_gts_string_pattern)]

use event_broker_sdk::{
    ConsumerError, ConsumerGroupId, EventBrokerError, OffsetManagerError, StorageBackendError,
};

#[test]
fn display_interpolates_fields() {
    let e = EventBrokerError::EventTypeNotDeclared {
        type_id: "gts.x.y.v1~foo".into(),
        detail: "d".into(),
        instance: String::new(),
    };
    assert!(e.to_string().contains("gts.x.y.v1~foo"));

    let e = EventBrokerError::SequenceViolation {
        expected_previous: 42,
        detail: "d".into(),
        instance: String::new(),
    };
    assert!(e.to_string().contains("42"));

    let e = EventBrokerError::RateLimitExceeded {
        retry_after_secs: 30,
        detail: "d".into(),
        instance: String::new(),
    };
    assert!(e.to_string().contains("30"));
}

#[test]
fn from_conversions() {
    let storage_err = StorageBackendError::Internal("test".into());
    let broker_err = EventBrokerError::from(storage_err);
    assert!(matches!(broker_err, EventBrokerError::StorageBackend(_)));

    let offset_err = OffsetManagerError::Internal("test".into());
    let broker_err = EventBrokerError::from(offset_err);
    assert!(matches!(broker_err, EventBrokerError::OffsetManager(_)));
}

#[test]
fn consumer_error_alias() {
    let e: ConsumerError = EventBrokerError::Internal("test".into());
    assert!(!e.to_string().is_empty());
}

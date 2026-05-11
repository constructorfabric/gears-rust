//! Unit tests for the error model.
#![allow(unknown_lints, de0901_gts_string_pattern)]

use std::error::Error;

use event_broker_sdk::{ConsumerError, EventBrokerError, OffsetManagerError, StorageBackendError};

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

#[derive(Debug, thiserror::Error)]
#[error("db failed")]
struct DbFailure;

#[test]
fn offset_manager_error_preserves_source_through_broker_error() {
    let offset_err = OffsetManagerError::persist_failed("write offset", "upsert failed", DbFailure);
    assert_eq!(
        offset_err.source().map(ToString::to_string),
        Some("db failed".to_owned())
    );

    let broker_err = EventBrokerError::from(offset_err);
    let source = broker_err
        .source()
        .expect("broker error should expose offset error");
    assert_eq!(source.to_string(), "persist failed: write offset");
    assert_eq!(
        source.source().map(ToString::to_string),
        Some("db failed".to_owned())
    );
}

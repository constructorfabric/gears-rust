use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use serde_json::json;

use crate::settings::{PostgresSettings, SegmentRule};

fn parse(value: serde_json::Value) -> Result<PostgresSettings, BackendError> {
    let serde_json::Value::Object(map) = value else {
        panic!("settings fixture must be an object");
    };
    PostgresSettings::parse(&map)
}

fn detail(result: Result<PostgresSettings, BackendError>) -> String {
    match result {
        Err(BackendError::InvalidConfig { detail }) => detail,
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn only_the_url_is_required() {
    assert_eq!(
        parse(json!({ "url": "postgres://u:p@h/db" })).unwrap(),
        PostgresSettings {
            url: "postgres://u:p@h/db".to_owned(),
            schema: "evbk".to_owned(),
            pool_max_size: 10,
            pool_acquire_timeout: Duration::from_secs(5),
            maintenance_interval: Duration::from_secs(10),
            segment: SegmentRule::Events(1_000_000),
        }
    );
    assert_eq!(detail(parse(json!({}))), "`url` is required");
}

#[test]
fn every_setting_is_read() {
    assert_eq!(
        parse(json!({
            "url": "postgres://h/db",
            "schema": "events_1",
            "pool_max_size": 4,
            "pool_acquire_timeout_ms": 250,
            "maintenance_interval_ms": 500,
            "segment": { "period": "PT1H" },
            "pgbouncer_transaction_mode": false,
        }))
        .unwrap(),
        PostgresSettings {
            url: "postgres://h/db".to_owned(),
            schema: "events_1".to_owned(),
            pool_max_size: 4,
            pool_acquire_timeout: Duration::from_millis(250),
            maintenance_interval: Duration::from_millis(500),
            segment: SegmentRule::Period(Duration::from_hours(1)),
        }
    );
}

#[test]
fn bad_values_are_refused_without_quoting_them() {
    assert_eq!(detail(parse(json!({ "url": 7 }))), "`url` must be a string");
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "schema": "Robert'); DROP" })
        )),
        "`schema` must be a bare identifier ([a-z_][a-z0-9_]*) of at most 63 bytes"
    );
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "pool_max_size": "secret" })
        )),
        "`pool_max_size` must be a positive integer"
    );
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "segment": { "events": 1 } })
        )),
        "`segment` must be { events: <positive integer> } or { period: <ISO 8601 duration> }"
    );
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "password": "hunter2" })
        )),
        "`password` is not a postgres backend setting"
    );
}

#[test]
fn a_transaction_mode_pooler_is_refused() {
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "pgbouncer_transaction_mode": true })
        )),
        "`pgbouncer_transaction_mode` is not supported: LISTEN needs a session-pooled connection"
    );
}

#[test]
fn an_unset_variable_in_the_url_is_refused() {
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://u:${EB_PG_TEST_UNSET_VARIABLE_42}@h/db" })
        )),
        "`url` names an environment variable that is not set"
    );
}

#[test]
fn debug_redacts_the_url() {
    let settings = parse(json!({ "url": "postgres://u:hunter2@h/db" })).unwrap();
    let printed = format!("{settings:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(printed.contains("<redacted>"), "{printed}");
}

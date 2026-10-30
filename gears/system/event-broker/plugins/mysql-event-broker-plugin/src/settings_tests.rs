use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use serde_json::json;

use crate::settings::{MaintenanceSettings, MysqlSettings, Pools, SegmentRule};

fn parse(value: serde_json::Value) -> Result<MysqlSettings, BackendError> {
    let serde_json::Value::Object(map) = value else {
        panic!("settings fixture must be an object");
    };
    MysqlSettings::parse(&map)
}

fn detail(result: Result<MysqlSettings, BackendError>) -> String {
    match result {
        Err(BackendError::InvalidConfig { detail }) => detail,
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn only_the_url_is_required() {
    assert_eq!(
        parse(json!({ "url": "mysql://u:p@h/db" })).unwrap(),
        MysqlSettings {
            url: "mysql://u:p@h/db".to_owned(),
            database: "evbk".to_owned(),
            pools: Pools {
                main: 10,
                background: 2,
            },
            pool_acquire_timeout: Duration::from_secs(5),
            segment: SegmentRule::Events(1_000_000),
            poll_interval: Duration::from_millis(500),
            require_durable_server: true,
            maintenance: MaintenanceSettings {
                rescan_interval: Duration::from_mins(1),
                crosscheck_every: 10,
                ddl_lock_wait: Duration::from_secs(1),
            },
        }
    );
}

#[test]
fn every_setting_is_read() {
    assert_eq!(
        parse(json!({
            "url": "mysql://u:p@h/db",
            "database": "evbk_2",
            "pools": { "main": 4, "background": 3 },
            "pool_acquire_timeout_ms": 250,
            "segment": { "period": "PT1H" },
            "poll_interval_ms": 100,
            "require_durable_server": false,
            "maintenance": { "rescan_interval_ms": 200, "crosscheck_every": 3, "ddl_lock_wait_s": 2 },
        }))
        .unwrap(),
        MysqlSettings {
            url: "mysql://u:p@h/db".to_owned(),
            database: "evbk_2".to_owned(),
            pools: Pools {
                main: 4,
                background: 3,
            },
            pool_acquire_timeout: Duration::from_millis(250),
            segment: SegmentRule::Period(Duration::from_hours(1)),
            poll_interval: Duration::from_millis(100),
            require_durable_server: false,
            maintenance: MaintenanceSettings {
                rescan_interval: Duration::from_millis(200),
                crosscheck_every: 3,
                ddl_lock_wait: Duration::from_secs(2),
            },
        }
    );
}

#[test]
fn broken_rules_are_named_without_their_values() {
    let url = "mysql://u:secret@h/db";
    assert_eq!(
        [
            json!({}),
            json!({ "url": 7 }),
            json!({ "url": url, "colour": "red" }),
            json!({ "url": url, "database": "Evbk-secret" }),
            json!({ "url": url, "database": "d".repeat(65) }),
            json!({ "url": url, "pools": { "main": 0 } }),
            json!({ "url": url, "pools": { "background": 1 } }),
            json!({ "url": url, "pools": { "spare": 1 } }),
            json!({ "url": url, "segment": { "events": 1 } }),
            json!({ "url": url, "segment": { "period": "PT0S" } }),
            json!({ "url": url, "poll_interval_ms": 0 }),
            json!({ "url": url, "require_durable_server": "yes" }),
            json!({ "url": url, "maintenance": { "ddl_lock_wait_s": 0 } }),
            json!({ "url": url, "maintenance": { "detacher": 1 } }),
        ]
        .map(|value| detail(parse(value))),
        [
            "`url` is required",
            "`url` must be a string",
            "`colour` is not a mysql backend setting",
            "`database` must be a bare identifier ([a-z_][a-z0-9_]*) of at most 64 bytes",
            "`database` must be a bare identifier ([a-z_][a-z0-9_]*) of at most 64 bytes",
            "`pools.main` must be a positive integer",
            "`pools.background` must be at least 2: the maintenance worker and the rescan",
            "`pools.spare` is not a mysql backend setting",
            "`segment` must be { events: <integer of at least 1000> } or { period: <ISO 8601 duration> }",
            "`segment` must be { events: <integer of at least 1000> } or { period: <ISO 8601 duration> }",
            "`poll_interval_ms` must be an integer from 10 to 60000",
            "`require_durable_server` must be a boolean",
            "`maintenance.ddl_lock_wait_s` must be an integer from 1 to 60",
            "`maintenance.detacher` is not a mysql backend setting",
        ]
    );
}

#[test]
fn the_url_expands_variables_and_is_redacted() {
    // A variable no environment sets.
    let name = "EVBK_MYSQL_SETTINGS_TEST_PASSWORD";
    let parsed = parse(json!({ "url": format!("mysql://u:${{{name}}}@h/db") }));
    assert_eq!(
        detail(parsed),
        "`url` names an environment variable that is not set"
    );
    let parsed = parse(json!({ "url": "mysql://u:hunter2@h/db" })).unwrap();
    assert!(!format!("{parsed:?}").contains("hunter2"));
}

#[test]
fn a_url_the_driver_does_not_accept_is_refused_at_build() {
    assert_eq!(
        [
            json!({ "url": "not a url" }),
            json!({ "url": "postgres://u:secret@h/db" }),
            json!({ "url": "mysql://u:secret@h:notaport/db" }),
        ]
        .map(|value| detail(parse(value))),
        ["`url` must be a mysql:// url the driver accepts"; 3]
    );
}

#[test]
fn a_segment_holds_at_least_one_full_append() {
    let url = "mysql://u:p@h/db";
    assert_eq!(
        [
            json!({ "url": url, "segment": { "events": 2 } }),
            json!({ "url": url, "segment": { "events": 999 } }),
        ]
        .map(|value| detail(parse(value))),
        ["`segment` must be { events: <integer of at least 1000> } or { period: <ISO 8601 duration> }";
            2]
    );
    assert_eq!(
        parse(json!({ "url": url, "segment": { "events": 1000 } }))
            .unwrap()
            .segment,
        SegmentRule::Events(1000)
    );
}

#[test]
fn intervals_are_bounded_on_both_sides() {
    let url = "mysql://u:p@h/db";
    assert_eq!(
        [
            json!({ "url": url, "poll_interval_ms": 9 }),
            json!({ "url": url, "poll_interval_ms": 60_001 }),
            json!({ "url": url, "maintenance": { "rescan_interval_ms": 99 } }),
            json!({ "url": url, "maintenance": { "rescan_interval_ms": 86_400_001 } }),
        ]
        .map(|value| detail(parse(value))),
        [
            "`poll_interval_ms` must be an integer from 10 to 60000",
            "`poll_interval_ms` must be an integer from 10 to 60000",
            "`maintenance.rescan_interval_ms` must be an integer from 100 to 86400000",
            "`maintenance.rescan_interval_ms` must be an integer from 100 to 86400000",
        ]
    );
    let edges = parse(json!({
        "url": url,
        "poll_interval_ms": 10,
        "maintenance": { "rescan_interval_ms": 86_400_000 },
    }))
    .unwrap();
    assert_eq!(
        (edges.poll_interval, edges.maintenance.rescan_interval),
        (Duration::from_millis(10), Duration::from_hours(24))
    );
}

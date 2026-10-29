use std::path::PathBuf;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use serde_json::json;

use crate::settings::{SqliteSettings, Sweep, Synchronous};

fn parse(value: serde_json::Value) -> Result<SqliteSettings, BackendError> {
    let serde_json::Value::Object(map) = value else {
        panic!("settings fixture must be an object");
    };
    SqliteSettings::parse(&map)
}

fn detail(result: Result<SqliteSettings, BackendError>) -> String {
    match result {
        Err(BackendError::InvalidConfig { detail }) => detail,
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn only_the_path_is_required() {
    assert_eq!(
        parse(json!({ "path": "/var/lib/evbk/events.db" })).unwrap(),
        SqliteSettings {
            path: PathBuf::from("/var/lib/evbk/events.db"),
            synchronous: Synchronous::Full,
            busy_timeout: Duration::from_secs(5),
            connections: 4,
            pool_acquire_timeout: Duration::from_secs(5),
            sweep: Sweep {
                interval: Duration::from_secs(1),
                batch: 10_000,
            },
        }
    );
    assert_eq!(detail(parse(json!({}))), "`path` is required");
}

#[test]
fn every_setting_is_read() {
    assert_eq!(
        parse(json!({
            "path": "events.db",
            "synchronous": "normal",
            "busy_timeout_ms": 250,
            "connections": 9,
            "pool_acquire_timeout_ms": 750,
            "sweep": { "interval_ms": 100, "batch": 50 },
        }))
        .unwrap(),
        SqliteSettings {
            path: PathBuf::from("events.db"),
            synchronous: Synchronous::Normal,
            busy_timeout: Duration::from_millis(250),
            connections: 9,
            pool_acquire_timeout: Duration::from_millis(750),
            sweep: Sweep {
                interval: Duration::from_millis(100),
                batch: 50,
            },
        }
    );
}

#[test]
fn a_partial_sweep_keeps_the_other_default() {
    assert_eq!(
        parse(json!({ "path": "e.db", "sweep": { "batch": 7 } }))
            .unwrap()
            .sweep,
        Sweep {
            interval: Duration::from_secs(1),
            batch: 7,
        }
    );
}

#[test]
fn unknown_settings_are_refused_by_name() {
    assert_eq!(
        detail(parse(json!({ "path": "e.db", "url": "sqlite://x" }))),
        "`url` is not a sqlite backend setting"
    );
    assert_eq!(
        detail(parse(json!({ "path": "e.db", "sweep": { "every": 1 } }))),
        "`sweep.every` is not a sqlite backend setting"
    );
}

#[test]
fn bad_values_are_refused_without_quoting_them() {
    let cases = [
        (json!({ "path": 7 }), "`path` must be a string"),
        (
            json!({ "path": "" }),
            "`path` must be a file path, not empty, `:memory:` or a `file:` URI",
        ),
        (
            json!({ "path": ":memory:" }),
            "`path` must be a file path, not empty, `:memory:` or a `file:` URI",
        ),
        (
            json!({ "path": "file:secret-name.db?mode=rwc" }),
            "`path` must be a file path, not empty, `:memory:` or a `file:` URI",
        ),
        (
            json!({ "path": "e.db", "synchronous": "OFF" }),
            "`synchronous` must be \"full\" or \"normal\"",
        ),
        (
            json!({ "path": "e.db", "busy_timeout_ms": 0 }),
            "`busy_timeout_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "connections": -3 }),
            "`connections` must be a positive integer",
        ),
        (
            json!({ "path": "e.db", "connections": 5_000_000_000_u64 }),
            "`connections` must be a positive integer",
        ),
        (
            json!({ "path": "e.db", "pool_acquire_timeout_ms": "5s" }),
            "`pool_acquire_timeout_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "sweep": 5 }),
            "`sweep` must be { interval_ms, batch }",
        ),
        (
            json!({ "path": "e.db", "sweep": { "interval_ms": 0 } }),
            "`sweep.interval_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "sweep": { "batch": 0 } }),
            "`sweep.batch` must be a positive integer",
        ),
    ];
    for (settings, expected) in cases {
        assert_eq!(detail(parse(settings)), expected);
    }
}

#[test]
fn the_path_expands_environment_variables() {
    assert_eq!(
        parse(json!({ "path": "${EVBK_SQLITE_TEST_UNSET_DIR:-/data}/events.db" }))
            .unwrap()
            .path,
        PathBuf::from("/data/events.db")
    );
    assert_eq!(
        detail(parse(
            json!({ "path": "${EVBK_SQLITE_TEST_UNSET_DIR}/events.db" })
        )),
        "`path` names an environment variable that is not set"
    );
}

#[test]
fn bounds_past_what_sqlite_and_the_sweep_take_are_refused() {
    let cases = [
        (
            json!({ "path": "e.db", "busy_timeout_ms": 2_147_483_648_u64 }),
            "`busy_timeout_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "pool_acquire_timeout_ms": u64::MAX }),
            "`pool_acquire_timeout_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "sweep": { "interval_ms": 86_400_001 } }),
            "`sweep.interval_ms` must be a positive integer of at most 86400000 (one day)",
        ),
        (
            json!({ "path": "e.db", "sweep": { "batch": 100_001 } }),
            "`sweep.batch` must be a positive integer of at most 100000",
        ),
    ];
    for (settings, expected) in cases {
        assert_eq!(detail(parse(settings)), expected);
    }
    let at_the_bounds = parse(json!({
        "path": "e.db",
        "busy_timeout_ms": 86_400_000,
        "sweep": { "interval_ms": 86_400_000, "batch": 100_000 },
    }))
    .unwrap();
    assert_eq!(
        (at_the_bounds.busy_timeout, at_the_bounds.sweep.batch),
        (Duration::from_hours(24), 100_000)
    );
}

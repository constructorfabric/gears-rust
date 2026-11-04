use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use serde_json::json;

use crate::settings::{HeadNotify, MaintenanceSettings, Pools, PostgresSettings, SegmentRule};

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
            pools: Pools {
                main: 10,
                background: 6,
            },
            pool_acquire_timeout: Duration::from_secs(5),
            segment: SegmentRule::Events(1_000_000),
            head_notify: HeadNotify::InTransaction,
            maintenance: MaintenanceSettings {
                rescan_interval: Duration::from_mins(1),
                crosscheck_every: 10,
                extender: 2,
                sealer: 1,
                retention: 1,
                reconcile: 1,
                detacher: 1,
                warn_after: 5,
                stuck_after: 20,
                stuck_age: Duration::from_mins(5),
            },
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
            "pools": { "main": 12, "background": 15 },
            "pool_acquire_timeout_ms": 250,
            "segment": { "period": "PT1H" },
            "head_notify": "after_commit",
            "pgbouncer_transaction_mode": false,
            "maintenance": {
                "rescan_interval_ms": 5000,
                "crosscheck_every": 3,
                "extender": { "concurrency": 3 },
                "sealer": { "concurrency": 2 },
                "retention": { "concurrency": 2 },
                "reconcile": { "concurrency": 2 },
                "detacher": { "concurrency": 4 },
                "postponed": { "warn_after": 2, "stuck_after": 7, "stuck_age_ms": 60000 },
            },
        }))
        .unwrap(),
        PostgresSettings {
            url: "postgres://h/db".to_owned(),
            schema: "events_1".to_owned(),
            pools: Pools {
                main: 12,
                background: 15,
            },
            pool_acquire_timeout: Duration::from_millis(250),
            segment: SegmentRule::Period(Duration::from_hours(1)),
            head_notify: HeadNotify::AfterCommit,
            maintenance: MaintenanceSettings {
                rescan_interval: Duration::from_secs(5),
                crosscheck_every: 3,
                extender: 3,
                sealer: 2,
                retention: 2,
                reconcile: 2,
                detacher: 4,
                warn_after: 2,
                stuck_after: 7,
                stuck_age: Duration::from_mins(1),
            },
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
            json!({ "url": "postgres://h/db", "pools": { "main": "secret" } })
        )),
        "`pools.main` must be a positive integer"
    );
    assert_eq!(
        [
            json!({ "url": "postgres://h/db", "pools": 10 }),
            json!({ "url": "postgres://h/db", "pools": { "background": 0 } }),
            json!({ "url": "postgres://h/db", "pools": { "ingest": 10 } }),
        ]
        .map(|settings| detail(parse(settings))),
        [
            "`pools` must be { main, background }",
            "`pools.background` must be a positive integer",
            "`pools.ingest` is not a postgres backend setting",
        ]
    );
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "segment": { "events": 1 } })
        )),
        "`segment` must be { events: <positive integer> } or { period: <ISO 8601 duration> }"
    );
    assert_eq!(
        detail(parse(
            json!({ "url": "postgres://h/db", "head_notify": "later" })
        )),
        "`head_notify` must be \"in_transaction\" or \"after_commit\""
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

#[test]
fn bad_maintenance_values_are_refused_without_quoting_them() {
    let refused = |maintenance: serde_json::Value| {
        detail(parse(
            json!({ "url": "postgres://h/db", "maintenance": maintenance }),
        ))
    };
    assert_eq!(
        [
            refused(json!(7)),
            refused(json!({ "rescan_interval_ms": 0 })),
            refused(json!({ "crosscheck_every": "often" })),
            refused(json!({ "extender": 2 })),
            refused(json!({ "sealer": { "concurrency": 0 } })),
            refused(json!({ "retention": { "concurrency": 1, "burst": 2 } })),
            refused(json!({ "detacher": { "workers": 1 } })),
            refused(json!({ "postponed": [] })),
            refused(json!({ "postponed": { "warn_after": -1 } })),
            refused(json!({ "postponed": { "stuck_age_ms": 0 } })),
            refused(json!({ "postponed": { "forever": true } })),
            refused(json!({ "vacuum": true })),
        ],
        [
            "`maintenance` must be an object",
            "`maintenance.rescan_interval_ms` must be a positive integer",
            "`maintenance.crosscheck_every` must be a positive integer",
            "`maintenance.extender` must be { concurrency: <positive integer> }",
            "`maintenance.sealer` must be { concurrency: <positive integer> }",
            "`maintenance.retention` must be { concurrency: <positive integer> }",
            "`maintenance.detacher` must be { concurrency: <positive integer> }",
            "`maintenance.postponed` must be an object",
            "`maintenance.postponed.warn_after` must be a positive integer",
            "`maintenance.postponed.stuck_age_ms` must be a positive integer",
            "`maintenance.postponed.forever` is not a postgres backend setting",
            "`maintenance.vacuum` is not a postgres backend setting",
        ]
    );
}

#[test]
fn the_background_pool_holds_every_pooled_worker() {
    let short = "`pools.background` must be at least the maintenance's pooled workers: \
                 extender + sealer + retention + reconcile concurrency, plus one for the rescan";
    // Defaults: 2 + 1 + 1 + 1 + 1 = 6 pooled workers; the background pool
    // follows the workers unless given.
    let background = |settings| parse(settings).map(|parsed| parsed.pools.background);
    assert_eq!(
        [
            json!({ "url": "postgres://h/db" }),
            json!({ "url": "postgres://h/db", "pools": { "background": 6 } }),
            json!({ "url": "postgres://h/db", "maintenance": { "sealer": { "concurrency": 3 } } }),
            // The detacher has connections of its own.
            json!({ "url": "postgres://h/db", "maintenance": { "detacher": { "concurrency": 9 } } }),
        ]
        .map(|settings| background(settings).unwrap()),
        [6, 6, 8, 6]
    );
    assert_eq!(
        [
            json!({ "url": "postgres://h/db", "pools": { "background": 5 } }),
            json!({
                "url": "postgres://h/db",
                "pools": { "background": 6 },
                "maintenance": { "sealer": { "concurrency": 2 } },
            }),
        ]
        .map(|settings| detail(parse(settings))),
        [short, short]
    );
}

//! The gear's full `Migrator` (its own tables and the ingest outbox's) on every
//! engine the gear database may be: it applies to an empty database, a second
//! run under the same history applies nothing, and every migration's DDL runs
//! again over the existing tables without failing, which is what a deployment
//! whose history was lost, or a second history table, relies on. The SDK's
//! producer registration migration, which a producing gear runs on its own
//! database, applies once on every engine and round-trips a full row.
//!
//! Needs a Docker daemon for `PostgreSQL` and `MySQL`:
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test migrations_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use chrono::{DateTime, TimeZone, Utc};
use event_broker::infra::storage::migrations::Migrator;
use event_broker_sdk::producer::producer_registration_migrations;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement, Value,
};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_gear;
use uuid::Uuid;

use support::matrix::{Engine, Matrix};

/// In the order the runner applies them.
const EVERY_MIGRATION: [&str; 4] = [
    "m001_create_toolkit_outbox_schema__event_broker__ob_default",
    "m20260819_000001_spec_cache",
    "m20260819_000002_storage_facade",
    "m20260819_000003_backend_binding",
];

async fn migrations_apply_once_and_rerun_cleanly(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "migrations").await else {
        return;
    };
    let gear_db = matrix.gear_db().open().await.db;

    let first = run_migrations_for_gear(&gear_db, "event-broker", Migrator::migrations())
        .await
        .expect("the migrations apply to an empty database");
    assert_eq!(
        (first.applied, first.skipped, first.applied_names),
        (
            EVERY_MIGRATION.len(),
            0,
            EVERY_MIGRATION.map(str::to_owned).to_vec()
        )
    );

    let again = run_migrations_for_gear(&gear_db, "event-broker", Migrator::migrations())
        .await
        .expect("a second run under the same history succeeds");
    assert_eq!(
        (again.applied, again.skipped, again.applied_names),
        (0, EVERY_MIGRATION.len(), Vec::<String>::new())
    );

    // A history of its own: every migration's DDL runs again over the tables
    // the first run created.
    let rerun = run_migrations_for_gear(&gear_db, "event-broker-rerun", Migrator::migrations())
        .await
        .expect("every migration re-applies over existing tables");
    assert_eq!(
        (rerun.applied, rerun.skipped, rerun.applied_names),
        (
            EVERY_MIGRATION.len(),
            0,
            EVERY_MIGRATION.map(str::to_owned).to_vec()
        )
    );
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_sqlite() {
    migrations_apply_once_and_rerun_cleanly(Engine::Sqlite).await;
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_postgres() {
    migrations_apply_once_and_rerun_cleanly(Engine::Postgres).await;
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_mysql() {
    migrations_apply_once_and_rerun_cleanly(Engine::Mysql).await;
}

/// One row of the SDK's `event_broker_producer_registrations` table, read back
/// with the types its entity binds.
#[derive(Debug, PartialEq, Eq, FromQueryResult)]
struct RegistrationRow {
    registration_key: String,
    queue: String,
    outbox_partition: i64,
    producer_id: Uuid,
    mode: String,
    client_agent: String,
    generation: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn insert_registration(conn: &DatabaseConnection, row: &RegistrationRow) {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Postgres => {
            "INSERT INTO event_broker_producer_registrations
                (registration_key, queue, outbox_partition, producer_id, mode,
                 client_agent, generation, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
        }
        _ => {
            "INSERT INTO event_broker_producer_registrations
                (registration_key, queue, outbox_partition, producer_id, mode,
                 client_agent, generation, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
    };
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        sql,
        [
            Value::from(row.registration_key.clone()),
            Value::from(row.queue.clone()),
            Value::from(row.outbox_partition),
            Value::from(row.producer_id),
            Value::from(row.mode.clone()),
            Value::from(row.client_agent.clone()),
            Value::from(row.generation),
            Value::from(row.created_at),
            Value::from(row.updated_at),
        ],
    ))
    .await
    .expect("insert a producer registration row");
}

/// The SDK's producer registration migration on every engine: it applies once,
/// a second run skips it, a full row round-trips with its UUID and microsecond
/// UTC timestamps intact, and two registration keys that differ only in case
/// are two rows, which `MySQL`'s case-insensitive default collation would
/// otherwise refuse as one key.
async fn the_sdk_registration_migration_applies_once_and_round_trips_its_rows(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "sdk-registrations").await else {
        return;
    };
    let gear_db = matrix.gear_db().open().await;

    let first = run_migrations_for_gear(
        &gear_db.db,
        "event-broker-sdk-producer",
        producer_registration_migrations(),
    )
    .await
    .expect("the producer registration migration applies to an empty database");
    assert_eq!(
        (first.applied, first.skipped, first.applied_names),
        (
            1,
            0,
            vec!["m001_create_event_broker_producer_registrations".to_owned()]
        )
    );

    let again = run_migrations_for_gear(
        &gear_db.db,
        "event-broker-sdk-producer",
        producer_registration_migrations(),
    )
    .await
    .expect("a second run under the same history succeeds");
    assert_eq!(
        (again.applied, again.skipped, again.applied_names),
        (0, 1, Vec::<String>::new())
    );

    let conn = sea_orm::Database::connect(gear_db.dsn.as_str())
        .await
        .expect("connect to the gear database");
    let at = Utc.with_ymd_and_hms(2026, 11, 3, 12, 34, 56).unwrap()
        + chrono::Duration::microseconds(123_456);
    let upper = RegistrationRow {
        registration_key: "Orders".to_owned(),
        queue: "billing".to_owned(),
        outbox_partition: 3,
        producer_id: Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0001),
        mode: "monotonic".to_owned(),
        client_agent: "orders-service/1.4.2".to_owned(),
        generation: 1,
        created_at: at,
        updated_at: at,
    };
    let lower = RegistrationRow {
        registration_key: "orders".to_owned(),
        queue: "billing".to_owned(),
        outbox_partition: 3,
        producer_id: Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0002),
        mode: "stateless".to_owned(),
        client_agent: "orders-service/1.4.2".to_owned(),
        generation: 2,
        created_at: at,
        updated_at: at,
    };
    insert_registration(&conn, &upper).await;
    insert_registration(&conn, &lower).await;

    let rows = RegistrationRow::find_by_statement(Statement::from_string(
        conn.get_database_backend(),
        "SELECT registration_key, queue, outbox_partition, producer_id, mode,
                client_agent, generation, created_at, updated_at
         FROM event_broker_producer_registrations
         ORDER BY generation",
    ))
    .all(&conn)
    .await
    .expect("read the producer registration rows");
    assert_eq!(rows, vec![upper, lower]);
}

#[tokio::test]
async fn the_sdk_registration_migration_applies_once_and_round_trips_its_rows_on_sqlite() {
    the_sdk_registration_migration_applies_once_and_round_trips_its_rows(Engine::Sqlite).await;
}

#[tokio::test]
async fn the_sdk_registration_migration_applies_once_and_round_trips_its_rows_on_postgres() {
    the_sdk_registration_migration_applies_once_and_round_trips_its_rows(Engine::Postgres).await;
}

#[tokio::test]
async fn the_sdk_registration_migration_applies_once_and_round_trips_its_rows_on_mysql() {
    the_sdk_registration_migration_applies_once_and_round_trips_its_rows(Engine::Mysql).await;
}

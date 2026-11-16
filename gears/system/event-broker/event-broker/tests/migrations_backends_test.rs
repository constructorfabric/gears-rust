//! The gear's full `Migrator` (its own tables and the ingest outbox's) on every
//! engine the gear database may be: it applies to an empty database, a second
//! run under the same history applies nothing, and every migration's DDL runs
//! again over the existing tables without failing, which is what a deployment
//! whose history was lost, or a second history table, relies on. The SDK's
//! producer registration migration, which a producing gear runs on its own
//! database, applies once on every engine and round-trips a full row. The
//! spec cache answers a topic's event types through its `(kind, topic)` index,
//! and which of a list of ids are registered on a topic, on every engine.
//!
//! Needs a Docker daemon for `PostgreSQL` and `MySQL`:
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test migrations_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use event_broker::domain::specification::{
    EventTypeCandidate, EventTypeCandidates, SpecificationManager,
};
use event_broker::infra::specification::{TypesRegistrySpecificationManager, bulk_load};
use event_broker::infra::storage::migrations::Migrator;
use event_broker::test_support::StaticTypesRegistry;
use event_broker::test_support::module_boot::migrated_gear_db;
use event_broker::test_support::type_registry::registry_holding;
use event_broker_sdk::producer::producer_registration_migrations;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement, Value,
};
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use toolkit_db::migration_runner::run_migrations_for_gear;
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::PlatformTypesRegistryApi;
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
    // The whole opened database, not only its pool: dropping it removes a
    // `SQLite` file the test still uses.
    let opened = matrix.gear_db().open().await;

    let first = run_migrations_for_gear(&opened.db, "event-broker", Migrator::migrations())
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

    let again = run_migrations_for_gear(&opened.db, "event-broker", Migrator::migrations())
        .await
        .expect("a second run under the same history succeeds");
    assert_eq!(
        (again.applied, again.skipped, again.applied_names),
        (0, EVERY_MIGRATION.len(), Vec::<String>::new())
    );

    // A history of its own: every migration's DDL runs again over the tables
    // the first run created.
    let rerun = run_migrations_for_gear(&opened.db, "event-broker-rerun", Migrator::migrations())
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

const TOPIC_A: &str = "gts.cf.core.events.topic.v1~x.eb.cand.a.v1";
const TOPIC_B: &str = "gts.cf.core.events.topic.v1~x.eb.cand.b.v1";
const TOPIC_NO_TYPES: &str = "gts.cf.core.events.topic.v1~x.eb.cand.none.v1";
const TYPE_A1: &str = "gts.cf.core.events.event.v1~x.eb.cand.a1.v1~";
const TYPE_A2: &str = "gts.cf.core.events.event.v1~x.eb.cand.a2.v1~";
const TYPE_B1: &str = "gts.cf.core.events.event.v1~x.eb.cand.b1.v1~";
const SUBJECT_S1: &str = "gts.x.eb.cand.s1.v1~";
const SUBJECT_S2_ANY: &str = "gts.x.eb.cand.s2.v1~acme.*";

fn candidate(id: &str, allowed_subject_types: &[&str]) -> EventTypeCandidate {
    EventTypeCandidate {
        id: gts::GtsTypeId::try_new(id).unwrap(),
        allowed_subject_types: allowed_subject_types
            .iter()
            .map(|pattern| gts::GtsIdPattern::try_new(pattern).unwrap())
            .collect(),
    }
}

/// The candidate query on the engine's own DDL: the `topic` filter, the
/// `gts_id` order and the decoded subject type lists.
async fn the_spec_cache_returns_a_topics_event_type_ids(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "spec-cache-candidates").await else {
        return;
    };
    let migrated = migrated_gear_db(&matrix.gear_db()).await;
    let db = Arc::clone(migrated.db());
    let fixtures = StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": TOPIC_B, "partitions": 1 },
        { "id": TOPIC_NO_TYPES, "partitions": 1 },
        { "id": TYPE_A2, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_S1, SUBJECT_S2_ANY] },
        { "id": TYPE_B1, "topic": TOPIC_B, "allowed_subject_types": [SUBJECT_S2_ANY] },
        { "id": TYPE_A1, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_S1] },
    ]));
    let topics: Vec<serde_json::Value> = [TOPIC_A, TOPIC_B, TOPIC_NO_TYPES]
        .iter()
        .map(|id| json!({ "id": id, "description": "a topic this test reads" }))
        .collect();
    let client: Arc<dyn PlatformTypesRegistryApi> =
        registry_holding(&topics, fixtures.event_type_schemas());
    bulk_load(&client, &db, &StaticTypesRegistry::empty_config())
        .await
        .expect("bulk_load");
    let manager = TypesRegistrySpecificationManager::new(db);
    let candidates_of = |topic: &'static str| {
        let manager = &manager;
        async move {
            manager
                .get_event_type_candidates(&GtsInstanceId::try_new(topic).unwrap())
                .await
                .expect("the cache answers")
        }
    };

    assert_eq!(
        candidates_of(TOPIC_A).await,
        EventTypeCandidates::Listed(vec![
            candidate(TYPE_A1, &[SUBJECT_S1]),
            candidate(TYPE_A2, &[SUBJECT_S1, SUBJECT_S2_ANY]),
        ])
    );
    assert_eq!(
        candidates_of(TOPIC_B).await,
        EventTypeCandidates::Listed(vec![candidate(TYPE_B1, &[SUBJECT_S2_ANY])])
    );
    assert_eq!(
        candidates_of(TOPIC_NO_TYPES).await,
        EventTypeCandidates::Listed(Vec::new())
    );
}

#[tokio::test]
async fn the_spec_cache_returns_a_topics_event_type_ids_on_sqlite() {
    the_spec_cache_returns_a_topics_event_type_ids(Engine::Sqlite).await;
}

#[tokio::test]
async fn the_spec_cache_returns_a_topics_event_type_ids_on_postgres() {
    the_spec_cache_returns_a_topics_event_type_ids(Engine::Postgres).await;
}

#[tokio::test]
async fn the_spec_cache_returns_a_topics_event_type_ids_on_mysql() {
    the_spec_cache_returns_a_topics_event_type_ids(Engine::Mysql).await;
}

/// The lookup over the cap on the engine's own DDL: the `topic` filter keeps
/// a type of another topic out, and an id nobody registered is not returned.
async fn the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic(engine: Engine) {
    const TYPE_X: &str = "gts.cf.core.events.event.v1~x.eb.cand.x.v1~";
    let Some(matrix) = Matrix::start(engine, "spec-cache-registered").await else {
        return;
    };
    let migrated = migrated_gear_db(&matrix.gear_db()).await;
    let db = Arc::clone(migrated.db());
    let fixtures = StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": TOPIC_B, "partitions": 1 },
        { "id": TYPE_A1, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_S1] },
        { "id": TYPE_A2, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_S1] },
        { "id": TYPE_B1, "topic": TOPIC_B, "allowed_subject_types": [SUBJECT_S1] },
    ]));
    let topics: Vec<serde_json::Value> = [TOPIC_A, TOPIC_B]
        .iter()
        .map(|id| json!({ "id": id, "description": "a topic this test reads" }))
        .collect();
    let client: Arc<dyn PlatformTypesRegistryApi> =
        registry_holding(&topics, fixtures.event_type_schemas());
    bulk_load(&client, &db, &StaticTypesRegistry::empty_config())
        .await
        .expect("bulk_load");
    let manager = TypesRegistrySpecificationManager::new(db);
    let type_ids = |ids: &[&str]| -> Vec<gts::GtsTypeId> {
        ids.iter()
            .map(|id| gts::GtsTypeId::try_new(id).unwrap())
            .collect()
    };

    let registered = manager
        .get_registered_event_types(
            &GtsInstanceId::try_new(TOPIC_A).unwrap(),
            &type_ids(&[TYPE_A1, TYPE_B1, TYPE_X]),
        )
        .await
        .expect("the cache answers");

    assert_eq!(registered, type_ids(&[TYPE_A1]));
}

#[tokio::test]
async fn the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic_on_sqlite() {
    the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic(Engine::Sqlite).await;
}

#[tokio::test]
async fn the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic_on_postgres() {
    the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic(Engine::Postgres)
        .await;
}

#[tokio::test]
async fn the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic_on_mysql() {
    the_spec_cache_returns_which_of_the_given_ids_are_registered_on_the_topic(Engine::Mysql).await;
}

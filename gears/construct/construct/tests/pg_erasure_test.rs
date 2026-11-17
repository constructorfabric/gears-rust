#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Erasure's storage on `PostgreSQL`: the `m006` migration, whose `up()`
//! asks the database for its columns and indexes, and the erasure's mark,
//! which `finish` matches by the timestamp `marked` read back. The unit
//! suites run both on `SQLite`; the column and index lookups and the
//! timestamp's precision differ on `PostgreSQL`, so they run here too.
//!
//! Docker required: without it the suite skips, unless
//! `CONSTRUCT_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI sets
//! it. Run with `make test-integration LANE=construct-pg`, or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-construct --features integration --test pg_erasure_test
//! ```

use std::sync::Arc;

use construct::domain::deletion::{DeletionStore, KnownTenants, ProfileGenerations};
use construct::domain::subject_settings::SubjectKey;
use construct::infra::storage::deletion_repo::DeletionTables;
use construct::infra::storage::migrations::{Migrator, m006_erasure_retention};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use time::{Duration, OffsetDateTime};
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

/// A `PostgreSQL` container, the toolkit handle the store takes, and a plain
/// connection for the migrations and the rows the test reads. Owned by the
/// test, so the container goes when the test does.
struct PgFixture {
    _container: ContainerAsync<Postgres>,
    db: Db,
    conn: DatabaseConnection,
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
fn require_docker() -> bool {
    std::env::var_os("CONSTRUCT_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Docker is not there: skip locally, fail where a skip would pass vacuously.
fn skip<T>(why: &str) -> Option<T> {
    assert!(!require_docker(), "PostgreSQL erasure suite: {why}");
    eprintln!("skipping -- PostgreSQL erasure suite: {why}");
    None
}

async fn pg_fixture() -> Option<PgFixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "construct");
    let container = match request.start().await {
        Ok(container) => container,
        Err(e) => {
            return skip(&format!(
                "could not start a PostgreSQL container via testcontainers ({e}); install or \
                 start Docker to run this for real"
            ));
        }
    };
    let port = match container.get_host_port_ipv4(5432).await {
        Ok(port) => port,
        Err(e) => {
            return skip(&format!(
                "the container started but its port could not be resolved ({e}); is Docker \
                 healthy?"
            ));
        }
    };
    let url = format!("postgres://user:pass@127.0.0.1:{port}/construct");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(5),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("the toolkit connects to the container");
    let conn = Database::connect(&url)
        .await
        .expect("a plain connection to the container");
    Some(PgFixture {
        _container: container,
        db,
        conn,
    })
}

const SETTINGS: &str = "construct__subject_settings";

async fn generation_of(conn: &DatabaseConnection, key: SubjectKey) -> i64 {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "SELECT profile_generation FROM {SETTINGS} WHERE tenant_id = '{}' AND subject_id = '{}'",
            key.tenant_id, key.subject_id
        ),
    ))
    .await
    .expect("query")
    .expect("the row")
    .try_get_by_index(0)
    .expect("the generation")
}

/// `m006` on `PostgreSQL`: a row stored before takes generation 0, the
/// columns, the index and the tenants table are there, a second run changes
/// nothing, and the step back is refused while a subject was erased and runs
/// once none was.
#[tokio::test]
async fn m006_runs_twice_and_steps_back_only_while_no_subject_was_erased() {
    let Some(pg) = pg_fixture().await else {
        return;
    };
    let key = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };
    Migrator::up(&pg.conn, Some(5)).await.expect("up to m005");
    pg.conn
        .execute_unprepared(&format!(
            "INSERT INTO {SETTINGS} (tenant_id, subject_id, personalization_enabled) \
             VALUES ('{}', '{}', true)",
            key.tenant_id, key.subject_id
        ))
        .await
        .expect("a row under m005");

    Migrator::up(&pg.conn, None).await.expect("the upgrade");
    let manager = SchemaManager::new(&pg.conn);

    assert_eq!(generation_of(&pg.conn, key).await, 0);
    for column in [
        "profile_generation",
        "erasure_marked_at",
        "erasure_attempted_at",
    ] {
        assert!(
            manager.has_column(SETTINGS, column).await.unwrap(),
            "{column}"
        );
    }
    assert!(
        manager
            .has_index(
                "construct__record_ids",
                "idx_construct__record_ids__received"
            )
            .await
            .unwrap()
    );
    assert!(manager.has_table("construct__tenants").await.unwrap());
    m006_erasure_retention::Migration
        .up(&manager)
        .await
        .expect("a second run is a no-op");

    pg.conn
        .execute_unprepared(&format!("UPDATE {SETTINGS} SET profile_generation = 1"))
        .await
        .unwrap();
    let refused = m006_erasure_retention::Migration
        .down(&manager)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("1 subjects"), "{refused}");
    assert!(
        manager
            .has_column(SETTINGS, "profile_generation")
            .await
            .unwrap(),
        "nothing was dropped"
    );

    pg.conn
        .execute_unprepared(&format!("UPDATE {SETTINGS} SET profile_generation = 0"))
        .await
        .unwrap();
    m006_erasure_retention::Migration
        .down(&manager)
        .await
        .expect("the step back while no subject was erased");
    assert!(
        !manager
            .has_column(SETTINGS, "profile_generation")
            .await
            .unwrap()
    );
    assert!(!manager.has_table("construct__tenants").await.unwrap());
}

/// The mark on `PostgreSQL`: the time `marked` reads back, which keeps only
/// microseconds of a mark set to the nanosecond, finishes the erasure, and a
/// finish for another mark leaves it under way.
#[tokio::test]
async fn the_mark_read_back_finishes_its_erasure_and_no_other() {
    let Some(pg) = pg_fixture().await else {
        return;
    };
    Migrator::up(&pg.conn, None).await.expect("the migrations");
    let store = DeletionTables::new(Arc::new(DBProvider::new(pg.db.clone())));
    let key = SubjectKey {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
    };
    let scope = AccessScope::for_tenant(key.tenant_id);
    let now = OffsetDateTime::now_utc();

    store.mark(&scope, key, now).await.expect("the mark");
    let mark = store
        .marked(&scope, key)
        .await
        .unwrap()
        .expect("an erasure under way");
    assert_eq!(mark.generation, 0);
    let marked = mark.at;
    assert!(
        (marked - now).abs() < Duration::milliseconds(1),
        "{marked} for {now}"
    );
    assert_eq!(
        store
            .under_way(now + Duration::seconds(1), 10)
            .await
            .unwrap(),
        [key],
        "a stopped erasure is found by its mark"
    );

    assert!(
        !store
            .finish(&scope, key, marked - Duration::minutes(10))
            .await
            .unwrap(),
        "a run of another erasure finishes nothing"
    );
    assert_eq!(store.generation(key).await.unwrap(), 0);

    assert!(store.finish(&scope, key, marked).await.unwrap());
    assert_eq!(store.generation(key).await.unwrap(), 1);
    assert!(store.marked(&scope, key).await.unwrap().is_none());
    assert_eq!(generation_of(&pg.conn, key).await, 1);
}

/// The tenants retention sweeps on `PostgreSQL`: a noted tenant and a tenant
/// with a settings row, each once, and noting a tenant again changes
/// nothing.
#[tokio::test]
async fn the_tenants_are_the_noted_ones_and_those_with_a_row() {
    let Some(pg) = pg_fixture().await else {
        return;
    };
    Migrator::up(&pg.conn, None).await.expect("the migrations");
    let store = DeletionTables::new(Arc::new(DBProvider::new(pg.db.clone())));
    let (noted, with_row) = (Uuid::new_v4(), Uuid::new_v4());
    let key = SubjectKey {
        tenant_id: with_row,
        subject_id: Uuid::new_v4(),
    };

    store.note(noted).await.unwrap();
    store.note(noted).await.unwrap();
    store
        .mark(
            &AccessScope::for_tenant(with_row),
            key,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let tenants = store.tenants().await.unwrap();
    assert!(
        tenants.contains(&noted) && tenants.contains(&with_row),
        "{tenants:?}"
    );
    assert_eq!(tenants.len(), 2, "each once: {tenants:?}");
}

#![cfg(feature = "integration")]
// Created: 2026-10-02 by Virtuozzo International GmbH
// The fixture, the seed and the reader are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used)]
//! The migration chain on a real `PostgreSQL`: the backend every stand runs
//! on, and the one the `sqlite::memory:` suites beside the migrations cannot
//! stand in for. A statement is rendered once per backend, and what `SQLite`
//! accepts `PostgreSQL` may refuse — `m20260917_000001_value_type_namespace`
//! did, over a placeholder spelt `SQLite`'s way, and until this suite nothing
//! had run the chain on `PostgreSQL` short of a stand.
//!
//! The chain goes through `run_migrations_for_testing`, the toolkit runner a
//! stand's startup goes through, so what applies here is what boots. Docker
//! required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_migrations_test
//! ```

use std::collections::BTreeMap;

use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};
use uuid::Uuid;

/// The migration that repoints value types from the toolkit namespace at the
/// gears one.
const MOVE: &str = "m20260917_000001_value_type_namespace";

/// A run of the chain, as the toolkit runner takes it.
type Chain = Vec<Box<dyn MigrationTrait>>;
const OLD_BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
const NEW_BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";

/// A `PostgreSQL` container, the toolkit handle the runner takes, and a plain
/// connection for the rows the test writes and reads around the chain. Owned
/// by the test, so the container goes when the test does.
struct PgFixture {
    _container: ContainerAsync<Postgres>,
    db: Db,
    conn: DatabaseConnection,
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
/// CI sets `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1`; locally it is unset.
fn require_docker() -> bool {
    std::env::var_os("SETTINGS_SERVICE_PG_REQUIRE_DOCKER")
        .is_some_and(|v| v != "0" && !v.is_empty())
}

/// Docker is not there: skip locally, fail where a skip would pass vacuously.
fn skip<T>(why: &str) -> Option<T> {
    assert!(!require_docker(), "PostgreSQL migration suite: {why}");
    eprintln!("skipping -- PostgreSQL migration suite: {why}");
    None
}

/// Bring up a `testcontainers` `PostgreSQL` and connect to it twice: once as
/// the toolkit does, for the runner, and once plainly, for the rows.
async fn pg_fixture() -> Option<PgFixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "settings");
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

    let url = format!("postgres://user:pass@127.0.0.1:{port}/settings");
    let opts = ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&url, opts)
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

/// The chain split where the move sits: everything before it, and the move
/// with everything after it.
fn split_at_the_move() -> (Chain, Chain) {
    let mut from_the_move = Migrator::migrations();
    let at = from_the_move
        .iter()
        .position(|m| m.name() == MOVE)
        .expect("the move is in the chain");
    let before_the_move = from_the_move.drain(..at).collect();
    (before_the_move, from_the_move)
}

fn names(migrations: &Chain) -> Vec<String> {
    migrations.iter().map(|m| m.name().to_owned()).collect()
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql).await.expect("statement runs");
}

/// Rows a stand had written under the old namespace, and the ones the move
/// must leave alone. Ids are UUIDs, as the `PostgreSQL` columns demand.
async fn seed_rows_written_before_the_move(conn: &DatabaseConnection) {
    let category = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO categories (id, key, name)
             VALUES ('{category}', 'network', 'Network')"
        ),
    )
    .await;

    for (slug, value_type) in [
        ("old_one", OLD_BOOL),
        ("old_two", "gts.cf.toolkit.settings.type_port.v1~"),
        ("already_moved", NEW_BOOL),
        // A third-party value type that merely resembles ours: the move is
        // anchored at the start of the id, so this must be left alone.
        ("foreign", "gts.acme.settings.type_bool_flag.v1~"),
        // Our namespace, but not our catalogue: `_` is a LIKE wildcard, so a
        // pattern that does not escape it would take `typeX` for `type_`.
        ("look_alike", "gts.cf.toolkit.settings.typeXflag.v1~"),
    ] {
        let id = Uuid::new_v4();
        exec(
            conn,
            &format!(
                "INSERT INTO setting_declarations
                   (id, key, leaf_slug, value_type_id, category_id, default_value,
                    scope_class, created_by)
                 VALUES ('{id}',
                         'gts.cf.core.settings.setting_type.v1~acme.settings.network.{slug}.v1~',
                         '{slug}', '{value_type}', '{category}', 'true', 'local', 'fixture')"
            ),
        )
        .await;
    }
}

/// Every declaration's value type, keyed by its slug.
async fn value_types(conn: &DatabaseConnection) -> BTreeMap<String, String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT leaf_slug, value_type_id FROM setting_declarations",
    ))
    .await
    .expect("readable")
    .iter()
    .map(|row| {
        (
            row.try_get::<String>("", "leaf_slug").expect("a slug"),
            row.try_get::<String>("", "value_type_id").expect("an id"),
        )
    })
    .collect()
}

/// A stand provisioned from scratch: the chain applies on an empty database,
/// a declaration written before the move reads back under the gears namespace
/// after it, and a restart finds nothing outstanding.
#[tokio::test]
async fn the_chain_applies_on_an_empty_database_and_moves_the_rows_written_before_it() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    let (before_the_move, from_the_move) = split_at_the_move();
    let expected_before = names(&before_the_move);
    let expected_from = names(&from_the_move);

    let applied = run_migrations_for_testing(&fixture.db, before_the_move)
        .await
        .expect("the migrations before the move apply");
    assert_eq!(applied.applied_names, expected_before);
    seed_rows_written_before_the_move(&fixture.conn).await;

    let applied = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the move and the rest of the chain apply");
    assert_eq!(applied.applied_names, expected_from);

    let after = value_types(&fixture.conn).await;
    assert_eq!(after["old_one"], NEW_BOOL);
    assert_eq!(after["old_two"], "gts.cf.core.settings.type_port.v1~");
    assert_eq!(after["already_moved"], NEW_BOOL);
    assert_eq!(after["foreign"], "gts.acme.settings.type_bool_flag.v1~");
    assert_eq!(after["look_alike"], "gts.cf.toolkit.settings.typeXflag.v1~");

    let again = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("a restart finds nothing outstanding");
    assert_eq!(again.applied, 0);
    assert_eq!(value_types(&fixture.conn).await, after);
}

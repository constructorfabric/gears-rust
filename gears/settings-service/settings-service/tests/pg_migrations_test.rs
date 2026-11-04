#![cfg(feature = "integration")]
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
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_migrations_test
//! ```

use std::collections::BTreeMap;

use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use toolkit_db::migration_runner::run_migrations_for_testing;
use uuid::Uuid;

mod common;
use common::pg_fixture;

/// The migration that repoints value types from the toolkit namespace at the
/// gears one.
const MOVE: &str = "m20260917_000001_value_type_namespace";

/// A run of the chain, as the toolkit runner takes it.
type Chain = Vec<Box<dyn MigrationTrait>>;
const OLD_BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
const NEW_BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";

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
    let Some(fixture) = pg_fixture("migration").await else {
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

/// The display-name migration on the backend it is written for: the column
/// is a nullable `character varying(256)`, and the trigram index that the
/// search `LIKE` leans on exists.
#[tokio::test]
async fn the_display_name_column_and_its_trigram_index_exist_after_the_chain() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");

    let column = fixture
        .conn
        .query_one_raw(Statement::from_string(
            fixture.conn.get_database_backend(),
            "SELECT data_type, character_maximum_length, is_nullable
             FROM information_schema.columns
             WHERE table_name = 'setting_declarations' AND column_name = 'display_name'",
        ))
        .await
        .expect("readable")
        .expect("the display_name column exists");
    assert_eq!(
        column.try_get::<String>("", "data_type").expect("a type"),
        "character varying"
    );
    assert_eq!(
        column
            .try_get::<i32>("", "character_maximum_length")
            .expect("a length"),
        256
    );
    assert_eq!(
        column.try_get::<String>("", "is_nullable").expect("a flag"),
        "YES",
        "display_name must be nullable"
    );

    let index = fixture
        .conn
        .query_all_raw(Statement::from_string(
            fixture.conn.get_database_backend(),
            "SELECT indexname FROM pg_indexes
             WHERE tablename = 'setting_declarations'
               AND indexname = 'idx_declarations_display_name_trgm'",
        ))
        .await
        .expect("readable");
    assert_eq!(index.len(), 1, "the trigram index on display_name exists");
}

/// The setter-tenant migration on the backend it is written for: each of the
/// three columns is a nullable `uuid`, so a row written before it reads
/// `NULL` and the upward disclosure boundary withholds its identity.
#[tokio::test]
async fn the_setter_tenant_columns_are_nullable_uuids_after_the_chain() {
    let Some(fixture) = pg_fixture("migration").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");

    for (table, column) in [
        ("setting_values", "set_by_tenant"),
        ("tenant_permissions", "set_by_tenant"),
        ("audit_records", "actor_tenant_id"),
    ] {
        let found = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                format!(
                    "SELECT data_type, is_nullable FROM information_schema.columns
                     WHERE table_name = '{table}' AND column_name = '{column}'"
                ),
            ))
            .await
            .expect("readable")
            .unwrap_or_else(|| panic!("`{table}.{column}` exists after the chain"));
        assert_eq!(
            found.try_get::<String>("", "data_type").expect("a type"),
            "uuid",
            "`{table}.{column}`"
        );
        assert_eq!(
            found.try_get::<String>("", "is_nullable").expect("a flag"),
            "YES",
            "`{table}.{column}` must be nullable"
        );
    }
}

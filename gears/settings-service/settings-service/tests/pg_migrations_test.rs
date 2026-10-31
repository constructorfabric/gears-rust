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

mod support;

use std::collections::BTreeMap;

use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
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
    _container: support::Container,
    db: Db,
    conn: DatabaseConnection,
}

/// Bring up a `testcontainers` `PostgreSQL` and connect to it twice: once as
/// the toolkit does, for the runner, and once plainly, for the rows.
async fn pg_fixture() -> Option<PgFixture> {
    let container = support::start("migration").await?;
    let opts = ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&container.url, opts)
        .await
        .expect("the toolkit connects to the container");
    let conn = Database::connect(&container.url)
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

/// The display-name migration on the backend it is written for: the column
/// is a nullable `character varying(256)`, and the trigram index that the
/// search `LIKE` leans on exists.
#[tokio::test]
async fn the_display_name_column_and_its_trigram_index_exist_after_the_chain() {
    let Some(fixture) = pg_fixture().await else {
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

/// The activation migration on the backend it is written for: both tables,
/// their unique constraints, the partial indexes, and the `audit_records`
/// additions — the widened `operation` vocabulary and the two nullable columns.
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
#[tokio::test]
async fn the_activation_tables_indexes_and_audit_additions_exist_after_the_chain() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies on an empty database");
    let backend = fixture.conn.get_database_backend();

    // The unique constraints.
    for constraint in ["uq_sub_subscriber_key_tenant", "uq_activation_state"] {
        let found = fixture
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT conname FROM pg_constraint WHERE conname = '{constraint}' AND contype = 'u'"
                ),
            ))
            .await
            .expect("readable");
        assert_eq!(found.len(), 1, "`{constraint}` exists");
    }

    // The indexes, and the two that are partial.
    let indexes = fixture
        .conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT indexname, indexdef FROM pg_indexes
             WHERE indexname IN ('idx_sub_key', 'idx_activation_key_tenant',
                                 'idx_activation_awaiting', 'idx_audit_change_ref')",
        ))
        .await
        .expect("readable");
    let defs: BTreeMap<String, String> = indexes
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "indexname").expect("a name"),
                row.try_get::<String>("", "indexdef").expect("a definition"),
            )
        })
        .collect();
    assert_eq!(defs.len(), 4, "{defs:?}");
    assert!(
        defs["idx_activation_awaiting"].contains("WHERE (status = 'awaiting'"),
        "partial over the awaiting rows: {}",
        defs["idx_activation_awaiting"]
    );
    assert!(
        defs["idx_audit_change_ref"].contains("WHERE (change_ref IS NOT NULL)"),
        "partial over the rows that carry a change_ref: {}",
        defs["idx_audit_change_ref"]
    );

    // The audit additions: nullable columns, and `activation` accepted.
    for (column, kind) in [("change_ref", "uuid"), ("detail", "text")] {
        let found = fixture
            .conn
            .query_one_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT data_type, is_nullable FROM information_schema.columns
                     WHERE table_name = 'audit_records' AND column_name = '{column}'"
                ),
            ))
            .await
            .expect("readable")
            .unwrap_or_else(|| panic!("`audit_records.{column}` exists"));
        assert_eq!(
            found.try_get::<String>("", "data_type").expect("type"),
            kind
        );
        assert_eq!(
            found.try_get::<String>("", "is_nullable").expect("flag"),
            "YES"
        );
    }
    // One `CHECK` constrains `operation`, validated, found by definition.
    let checks = fixture
        .conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT convalidated FROM pg_constraint
             WHERE conrelid = 'audit_records'::regclass AND contype = 'c'
               AND pg_get_constraintdef(oid) ~ '\\yoperation\\y'",
        ))
        .await
        .expect("readable");
    assert_eq!(checks.len(), 1, "one CHECK on `operation`");
    assert!(checks[0].try_get::<bool>("", "convalidated").expect("flag"));
    let id = Uuid::new_v4();
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO audit_records
               (id, resource, declaration_key, operation, actor, actor_classification,
                outcome, request_id, change_ref, detail)
             VALUES ('{id}', 'cf.settings:k@t', 'k', 'activation', 'gear', 'public',
                     'failure', 'req', '{}', 'could not apply')",
            Uuid::new_v4()
        ),
    )
    .await;
    assert!(
        fixture
            .conn
            .execute_unprepared(&format!(
                "INSERT INTO audit_records
                   (id, resource, declaration_key, operation, actor, actor_classification,
                    outcome, request_id)
                 VALUES ('{}', 'cf.settings:k@t', 'k', 'delete', 'gear', 'public', 'success', 'req')",
                Uuid::new_v4()
            ))
            .await
            .is_err(),
        "an operation outside the vocabulary is still refused"
    );

    // A state's status is checked and defaults to `awaiting`.
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO activation_state (subscriber, key, tenant, change_ref, expected_value)
             VALUES ('gear', 'k', 't', '{}', 'true')",
            Uuid::new_v4()
        ),
    )
    .await;
    let status = fixture
        .conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT status FROM activation_state WHERE subscriber = 'gear'",
        ))
        .await
        .expect("readable")
        .expect("the row");
    assert_eq!(
        status.try_get::<String>("", "status").expect("status"),
        "awaiting"
    );
}

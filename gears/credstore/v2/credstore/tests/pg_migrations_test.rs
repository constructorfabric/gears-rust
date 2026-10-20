// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
#![cfg(feature = "integration")]
// The fixture, the seed and the readers are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used)]
//! `m0002_value_versions` on a real `PostgreSQL`: the backend every stand runs
//! on, and the one the `sqlite::memory:` suite beside the migration cannot
//! stand in for. The migration is rendered once per backend — in place on
//! `PostgreSQL` (constraint look-ups in `pg_constraint`, `DO` blocks, `ALTER
//! TABLE`), as a table rebuild on `SQLite` — so the `SQLite` tests prove none of
//! the `PostgreSQL` arm, and a statement `PostgreSQL` refuses would reach a
//! stand first.
//!
//! The migration is permanent schema history, so this suite is permanent too.
//! It stands in for a database as `m0001` shipped it (rows in every status,
//! with and without value fingerprints) and runs the upgrade through
//! `run_migrations_for_testing`, the toolkit runner a stand's startup goes
//! through. It reads the guard's marker through the gear's own constants and
//! never touches the one-off value-migration tool.
//!
//! Docker required: without it the suite skips, unless
//! `CREDSTORE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI sets
//! it. Run via `make test-integration LANE=credstore-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-credstore-v2 --features integration --test pg_migrations_test
//! ```

use std::collections::BTreeSet;

use credstore_v2::infra::storage::migrations::m0002_value_versions::{
    VALUE_MIGRATION_COPY_DONE_PHASES, VALUE_MIGRATION_STATE_TABLE, VALUES_NOT_MIGRATED,
};
use credstore_v2::infra::storage::migrations::{Migrator, m0001_initial_schema, m0002_value_versions};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::{MigrationError, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, Db, connect_db};
use uuid::Uuid;

/// The name `m0002` goes by in the migration history.
const M0002: &str = "m0002_value_versions";

/// A `PostgreSQL` container, the toolkit handle the runner takes, and a plain
/// connection for the rows the test writes and reads around the chain. Owned
/// by the test, so the container goes when the test does.
struct PgFixture {
    _container: ContainerAsync<Postgres>,
    db: Db,
    conn: DatabaseConnection,
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
/// CI sets `CREDSTORE_PG_REQUIRE_DOCKER=1`; locally it is unset.
fn require_docker() -> bool {
    std::env::var_os("CREDSTORE_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
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
        .with_env_var("POSTGRES_DB", "credstore");
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

    let url = format!("postgres://user:pass@127.0.0.1:{port}/credstore");
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

/// The database as `m0001` shipped it: the chain stopped before `m0002`.
fn shipped_chain() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(m0001_initial_schema::Migration)]
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql).await.expect("statement runs");
}

async fn query(conn: &DatabaseConnection, sql: &str) -> Vec<sea_orm::QueryResult> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect("readable")
}

/// Whether a catalog query finds at least one row.
async fn exists(conn: &DatabaseConnection, sql: &str) -> bool {
    !query(conn, sql).await.is_empty()
}

async fn table_exists(conn: &DatabaseConnection, table: &str) -> bool {
    exists(
        conn,
        &format!(
            "SELECT 1 FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = '{table}' LIMIT 1"
        ),
    )
    .await
}

async fn index_exists(conn: &DatabaseConnection, index: &str) -> bool {
    exists(
        conn,
        &format!(
            "SELECT 1 FROM pg_indexes \
             WHERE schemaname = current_schema() AND indexname = '{index}' LIMIT 1"
        ),
    )
    .await
}

async fn columns(conn: &DatabaseConnection, table: &str) -> BTreeSet<String> {
    query(
        conn,
        &format!(
            "SELECT column_name::text AS column_name FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = '{table}'"
        ),
    )
    .await
    .iter()
    .map(|row| row.try_get::<String>("", "column_name").expect("a name"))
    .collect()
}

fn names(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|s| (*s).to_owned()).collect()
}

/// The columns `credstore_secrets` has as `m0001` shipped it.
fn shipped_columns() -> BTreeSet<String> {
    names(&[
        "id",
        "tenant_id",
        "reference",
        "sharing",
        "owner_id",
        "status",
        "created_at",
        "updated_at",
        "version",
        "secret_type_uuid",
        "expires_at",
        "value_fp",
        "fp_key_id",
    ])
}

/// The columns `credstore_secrets` has once `m0002` ran: the fingerprint fence
/// is gone, the value pointer and the suppression policy are in.
fn migrated_columns() -> BTreeSet<String> {
    names(&[
        "id",
        "tenant_id",
        "reference",
        "sharing",
        "owner_id",
        "status",
        "created_at",
        "updated_at",
        "version",
        "secret_type_uuid",
        "expires_at",
        "value_version",
        "fallback",
    ])
}

/// A row as a database stood on `m0001` can hold it.
struct ShippedRow {
    id: Uuid,
    reference: &'static str,
    /// `1` private, `2` tenant, `3` shared.
    sharing: i16,
    /// `1` provisioning, `2` active, `3` deprovisioning.
    status: i16,
    /// The value-fingerprint fence: set on API-written rows, `NULL` on rows
    /// seeded out of band.
    fingerprint: bool,
    version: i64,
    owner_id: Uuid,
    secret_type_uuid: Uuid,
    /// UTC, `YYYY-MM-DD HH:MM:SS`.
    expires_at: Option<&'static str>,
}

const TENANT: Uuid = Uuid::from_u128(0xAAAA);

/// Rows in every status, in both key classes, with and without fingerprints.
fn shipped_rows() -> Vec<ShippedRow> {
    vec![
        ShippedRow {
            id: Uuid::from_u128(0x1001),
            reference: "active-private-fingerprinted",
            sharing: 1,
            status: 2,
            fingerprint: true,
            version: 3,
            owner_id: Uuid::from_u128(0xB001),
            secret_type_uuid: Uuid::from_u128(0xC001),
            expires_at: Some("2031-02-03 04:05:06"),
        },
        ShippedRow {
            id: Uuid::from_u128(0x1002),
            reference: "active-tenant-seeded",
            sharing: 2,
            status: 2,
            fingerprint: false,
            version: 1,
            owner_id: Uuid::from_u128(0xB002),
            secret_type_uuid: Uuid::from_u128(0xC002),
            expires_at: None,
        },
        ShippedRow {
            id: Uuid::from_u128(0x1003),
            reference: "active-shared-fingerprinted",
            sharing: 3,
            status: 2,
            fingerprint: true,
            version: 7,
            owner_id: Uuid::from_u128(0xB003),
            secret_type_uuid: Uuid::from_u128(0xC003),
            expires_at: None,
        },
        ShippedRow {
            id: Uuid::from_u128(0x1004),
            reference: "provisioning-tenant",
            sharing: 2,
            status: 1,
            fingerprint: true,
            version: 1,
            owner_id: Uuid::from_u128(0xB004),
            secret_type_uuid: Uuid::from_u128(0xC004),
            expires_at: None,
        },
        ShippedRow {
            id: Uuid::from_u128(0x1005),
            reference: "deprovisioning-private",
            sharing: 1,
            status: 3,
            fingerprint: false,
            version: 2,
            owner_id: Uuid::from_u128(0xB005),
            secret_type_uuid: Uuid::from_u128(0xC005),
            expires_at: None,
        },
    ]
}

async fn insert_shipped_rows(conn: &DatabaseConnection) {
    for row in shipped_rows() {
        let expires_at = row
            .expires_at
            .map_or_else(|| "NULL".to_owned(), |at| format!("'{at}+00'::timestamptz"));
        let (fingerprint, key_id) = if row.fingerprint {
            ("decode('00ff', 'hex')", "1")
        } else {
            ("NULL", "NULL")
        };
        exec(
            conn,
            &format!(
                "INSERT INTO credstore_secrets
                   (id, tenant_id, reference, sharing, owner_id, status, version,
                    secret_type_uuid, expires_at, value_fp, fp_key_id)
                 VALUES ('{}', '{TENANT}', '{}', {}, '{}', {}, {}, '{}', {expires_at},
                         {fingerprint}, {key_id})",
                row.id,
                row.reference,
                row.sharing,
                row.owner_id,
                row.status,
                row.version,
                row.secret_type_uuid,
            ),
        )
        .await;
    }
}

/// Record that the value migration finished copying, the way the guard looks
/// for it: the tool's progress table, holding a header row in `phase`. Only the
/// columns the guard reads are created.
async fn mark_value_migration(conn: &DatabaseConnection, phase: &str) {
    exec(
        conn,
        &format!(
            "CREATE TABLE {VALUE_MIGRATION_STATE_TABLE} \
             (phase TEXT NOT NULL, discard_values BOOLEAN NOT NULL)"
        ),
    )
    .await;
    exec(
        conn,
        &format!(
            "INSERT INTO {VALUE_MIGRATION_STATE_TABLE} (phase, discard_values) \
             VALUES ('{phase}', FALSE)"
        ),
    )
    .await;
}

/// A row of `credstore_secrets` as `m0002` leaves it.
#[derive(Debug, PartialEq, Eq)]
struct MigratedRow {
    id: String,
    reference: String,
    sharing: i16,
    status: i16,
    version: i64,
    owner_id: String,
    secret_type_uuid: String,
    expires_at: Option<String>,
    value_version: Option<String>,
    fallback: i16,
}

async fn migrated_rows(conn: &DatabaseConnection) -> Vec<MigratedRow> {
    query(
        conn,
        "SELECT id::text AS id, reference, sharing, status, version,
                owner_id::text AS owner_id, secret_type_uuid::text AS secret_type_uuid,
                to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS expires_at,
                value_version, fallback
         FROM credstore_secrets ORDER BY id",
    )
    .await
    .iter()
    .map(|row| MigratedRow {
        id: row.try_get("", "id").expect("id"),
        reference: row.try_get("", "reference").expect("reference"),
        sharing: row.try_get("", "sharing").expect("sharing"),
        status: row.try_get("", "status").expect("status"),
        version: row.try_get("", "version").expect("version"),
        owner_id: row.try_get("", "owner_id").expect("owner_id"),
        secret_type_uuid: row
            .try_get("", "secret_type_uuid")
            .expect("secret_type_uuid"),
        expires_at: row.try_get("", "expires_at").expect("expires_at"),
        value_version: row.try_get("", "value_version").expect("value_version"),
        fallback: row.try_get("", "fallback").expect("fallback"),
    })
    .collect()
}

/// `(id, status)` of every row, in id order: all a database still shaped like
/// `m0001` is read for.
async fn shipped_statuses(conn: &DatabaseConnection) -> Vec<(String, i16)> {
    query(
        conn,
        "SELECT id::text AS id, status FROM credstore_secrets ORDER BY id",
    )
    .await
    .iter()
    .map(|row| {
        (
            row.try_get("", "id").expect("id"),
            row.try_get("", "status").expect("status"),
        )
    })
    .collect()
}

/// Insert one row of the shape `m0002` leaves behind, to probe its `CHECK`s;
/// everything the probe does not vary is valid.
async fn insert_migrated_row(
    conn: &DatabaseConnection,
    status: i16,
    value_version: Option<&str>,
    fallback: i16,
) -> Result<(), DbErr> {
    let value_version = value_version.map_or_else(|| "NULL".to_owned(), |v| format!("'{v}'"));
    // A reference of its own, so the rows a test accepts do not collide on the
    // unique index of the tenant's non-private references.
    let id = Uuid::new_v4();
    conn.execute_unprepared(&format!(
        "INSERT INTO credstore_secrets
           (id, tenant_id, reference, sharing, owner_id, status, value_version, fallback,
            secret_type_uuid)
         VALUES ('{id}', '{TENANT}', 'probe-{id}', 2, '{}', {status}, {value_version}, {fallback},
                 '{}')",
        Uuid::new_v4(),
        Uuid::from_u128(0x00C0_FFEE),
    ))
    .await
    .map(|_| ())
}

/// A row the migrated schema must refuse, and the `CHECK` that refuses it.
struct Refused {
    status: i16,
    value_version: Option<&'static str>,
    fallback: i16,
    constraint: &'static str,
}

/// The state `m0002` creates, beyond the table reshaping each test reads.
async fn assert_m0002_schema(conn: &DatabaseConnection) {
    assert_eq!(columns(conn, "credstore_secrets").await, migrated_columns());
    assert_eq!(
        columns(conn, "credstore_write_intents").await,
        names(&[
            "attempt_id",
            "tenant_id",
            "record_id",
            "reference",
            "lease_until"
        ]),
        "the write-intent journal"
    );
    assert_eq!(
        columns(conn, "credstore_store_cleanup").await,
        names(&[
            "id",
            "tenant_id",
            "record_id",
            "op",
            "selector",
            "version",
            "created_at"
        ]),
        "the store cleanup debts"
    );
    for index in [
        "idx_credstore_write_intents_record",
        "idx_credstore_write_intents_ref",
        "idx_credstore_store_cleanup_record",
        "idx_credstore_type",
        "idx_credstore_lookup",
        "idx_credstore_expiry",
        "uq_credstore_nonprivate",
        "uq_credstore_private",
    ] {
        assert!(index_exists(conn, index).await, "index {index} must exist");
    }
    assert!(
        !index_exists(conn, "idx_credstore_write_intents_lease").await,
        "nothing scans the journal, so it has no lease index"
    );
    assert!(
        !index_exists(conn, "idx_credstore_pending").await,
        "the reaper's sweep index has nothing left to sweep"
    );
}

/// A database that shipped with rows in every status, whose values the
/// migration tool has moved: `m0002` drops the saga rows, demotes the active
/// ones to `declared`, drops the fingerprint fence and adds the value pointer,
/// the narrowed `CHECK`s and the write-intent journal.
#[tokio::test]
async fn m0002_postgres_migrates_shipped_rows() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, shipped_chain())
        .await
        .expect("m0001 applies");
    assert_eq!(
        columns(&fixture.conn, "credstore_secrets").await,
        shipped_columns()
    );
    insert_shipped_rows(&fixture.conn).await;
    // Any phase past copying lets the guard through (the `SQLite` suite walks
    // every one of them); what is proven here is the guard's own `PostgreSQL`
    // spelling.
    mark_value_migration(&fixture.conn, VALUE_MIGRATION_COPY_DONE_PHASES[0]).await;

    let applied = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("m0002 applies over the shipped rows");
    assert_eq!(applied.applied_names, vec![M0002.to_owned()]);
    assert_eq!(applied.skipped, 1, "m0001 is already in the history");

    // Saga rows (1, 3) are gone; every active row (2) survives as `declared`
    // (4) with no value pointer, the default suppression policy, and the
    // columns the migration does not own untouched.
    let expected: Vec<MigratedRow> = shipped_rows()
        .into_iter()
        .filter(|row| row.status == 2)
        .map(|row| MigratedRow {
            id: row.id.to_string(),
            reference: row.reference.to_owned(),
            sharing: row.sharing,
            status: 4,
            version: row.version,
            owner_id: row.owner_id.to_string(),
            secret_type_uuid: row.secret_type_uuid.to_string(),
            expires_at: row.expires_at.map(str::to_owned),
            value_version: None,
            fallback: 1,
        })
        .collect();
    assert_eq!(expected.len(), 3, "fixture: three active rows");
    assert_eq!(migrated_rows(&fixture.conn).await, expected);
    assert_m0002_schema(&fixture.conn).await;

    // The new CHECKs hold: the retired status, a pointer on a `declared` row,
    // no pointer on an `active` row, and an unknown suppression policy.
    let rejected = vec![
        Refused {
            status: 1,
            value_version: Some("v1"),
            fallback: 1,
            constraint: "credstore_secrets_status_check",
        },
        Refused {
            status: 2,
            value_version: None,
            fallback: 1,
            constraint: "credstore_secrets_value_version_check",
        },
        Refused {
            status: 4,
            value_version: Some("v1"),
            fallback: 1,
            constraint: "credstore_secrets_value_version_check",
        },
        Refused {
            status: 2,
            value_version: Some("v1"),
            fallback: 3,
            constraint: "ck_credstore_fallback",
        },
    ];
    for probe in rejected {
        let err = insert_migrated_row(
            &fixture.conn,
            probe.status,
            probe.value_version,
            probe.fallback,
        )
        .await
        .expect_err("the CHECK must refuse the row");
        assert!(
            err.to_string().contains(probe.constraint),
            "(status {}, value_version {:?}, fallback {}) must violate {}: {err}",
            probe.status,
            probe.value_version,
            probe.fallback,
            probe.constraint
        );
    }
    insert_migrated_row(&fixture.conn, 2, Some("v1"), 1)
        .await
        .expect("an `active` row with a pointer is what the code writes");
}

/// Rows written before the release whose values were not moved: `m0002`
/// refuses, names the tool, and leaves the database as `m0001` shipped it.
#[tokio::test]
async fn m0002_postgres_guard_refuses_unmigrated_active_rows() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, shipped_chain())
        .await
        .expect("m0001 applies");
    insert_shipped_rows(&fixture.conn).await;
    let before = shipped_statuses(&fixture.conn).await;
    assert_eq!(before.len(), shipped_rows().len());

    // Nothing recorded, and then a marker that says copying is not finished
    // (header present, phase not past copying, no discard decision): the same
    // refusal.
    for marker in [None, Some("copying")] {
        if let Some(phase) = marker {
            mark_value_migration(&fixture.conn, phase).await;
        }
        let err = run_migrations_for_testing(&fixture.db, Migrator::migrations())
            .await
            .expect_err("m0002 must refuse to run over unmigrated active rows");
        assert!(
            matches!(
                &err,
                MigrationError::MigrationFailed { migration, source, .. }
                    if migration == M0002
                        && matches!(source, DbErr::Migration(message) if message == VALUES_NOT_MIGRATED)
            ),
            "marker {marker:?}: expected the guard's refusal, got {err:?}"
        );
        assert!(
            VALUES_NOT_MIGRATED.contains("credstore-value-migration migrate"),
            "the error names the tool to run"
        );

        assert_eq!(
            columns(&fixture.conn, "credstore_secrets").await,
            shipped_columns(),
            "marker {marker:?}: the table is still m0001's"
        );
        assert_eq!(
            shipped_statuses(&fixture.conn).await,
            before,
            "marker {marker:?}: no row was touched"
        );
        assert!(
            !table_exists(&fixture.conn, "credstore_write_intents").await,
            "marker {marker:?}: nothing of m0002 was applied"
        );
    }
}

/// A new installation: no rows, so nothing to move and no tool to run.
#[tokio::test]
async fn m0002_postgres_fresh_install_passes() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, shipped_chain())
        .await
        .expect("m0001 applies");

    let applied = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("m0002 applies over an empty table without the tool's marker");
    assert_eq!(applied.applied_names, vec![M0002.to_owned()]);
    assert!(
        migrated_rows(&fixture.conn).await.is_empty(),
        "nothing was invented"
    );
    assert_m0002_schema(&fixture.conn).await;
    insert_migrated_row(&fixture.conn, 2, Some("v1"), 1)
        .await
        .expect("an `active` row with a pointer");
    insert_migrated_row(&fixture.conn, 4, None, 2)
        .await
        .expect("a `declared` row that suppresses");

    let again = run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("a restart finds nothing outstanding");
    assert_eq!(again.applied, 0);
}

/// `m0002` deletes saga rows and drops the fingerprints, so there is nothing a
/// `down` could restore: it fails, on the migration and through the migrator,
/// and changes nothing.
#[tokio::test]
async fn m0002_postgres_down_is_irreversible() {
    let Some(fixture) = pg_fixture().await else {
        return;
    };
    Migrator::up(&fixture.conn, None)
        .await
        .expect("the whole chain applies");

    let err = m0002_value_versions::Migration
        .down(&SchemaManager::new(&fixture.conn))
        .await
        .expect_err("m0002 cannot be rolled back");
    assert_irreversible(&err);
    assert_m0002_schema(&fixture.conn).await;

    let err = Migrator::down(&fixture.conn, Some(1))
        .await
        .expect_err("rolling back m0002 through the migrator must fail");
    assert_irreversible(&err);
    assert_m0002_schema(&fixture.conn).await;
}

fn assert_irreversible(err: &DbErr) {
    assert!(
        matches!(
            err,
            DbErr::Migration(message)
                if message.contains("m0002_value_versions is irreversible")
                    && message.contains("snapshot")
        ),
        "expected the irreversibility error, got {err:?}"
    );
}

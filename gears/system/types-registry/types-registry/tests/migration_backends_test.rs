//! Real up/down of the P0 initial migration on `PostgreSQL` and `MySQL`.
//!
//! `SQLite` is covered unconditionally by `migration_test.rs`; the other two
//! dialects — identity columns, `bytea` / `VARBINARY`, `COLLATE "C"` /
//! `ascii_bin`, `DATETIME(6)`, FK `RESTRICT`, and `MySQL`'s inline `KEY`
//! declarations — cannot be reached from `SQLite` at all. Each test spins up a
//! container, applies the migration, asserts the nine tables answer, checks the
//! two CHECK constraints the task names by hand, and rolls back.
//!
//! Gated behind `--features integration` because it needs a Docker daemon:
//!
//! ```text
//! cargo test -p cf-gears-types-registry --features integration --test migration_backends_test
//! ```
//!
//! `constraint-multi-backend` makes this a correctness requirement, not a
//! nice-to-have: the CHECK expressions lower differently on each engine
//! (`NOT dry_run` over `TINYINT(1)` versus a real `boolean`), so
//! `SQLite`-only evidence does not carry.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::time::Duration;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerRequest, ImageExt};
use toolkit_gts::gts_id;

use types_registry::infra::storage::Migrator;

const P0_TABLES: &[&str] = &[
    "types_registry__version_family",
    "types_registry__operation",
    "types_registry__operation_item",
    "types_registry__entity",
    "types_registry__type_schema_revision",
    "types_registry__instance_revision",
    "types_registry__type_schema",
    "types_registry__instance",
    "types_registry__dependency",
];

const OP_ID: &str = "00000000-0000-0000-0000-0000000000a1";
const PRINCIPAL: &str = "00000000-0000-0000-0000-0000000000b1";
const ENTITY_UUID: &str = "00000000-0000-0000-0000-0000000000c1";
const TS: &str = "2026-08-18 00:00:00";
const GTS_TYPE: &str = gts_id!("acme.crm.customer.type.v1~");
const FAMILY_KEY: &str = "gts.acme.crm.customer.type";

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

async fn exec(db: &DatabaseConnection, sql: impl Into<String>) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        sql.into(),
    ))
    .await
    .map(|_| ())
}

/// Everything the two backend tests assert once the schema is up: the nine
/// tables answer, `ck_tr_entity_owner` rejects a global entity with no
/// `owning_gear`, and `ck_tr_operation_item_state` rejects a succeeded
/// non-dry-run registration item with no `result_revision_no`.
async fn assert_schema_behaves(db: &DatabaseConnection) {
    let op_id = uuid_literal(db, OP_ID);
    let principal = uuid_literal(db, PRINCIPAL);
    let entity_uuid = uuid_literal(db, ENTITY_UUID);
    for table in P0_TABLES {
        exec(db, format!("SELECT COUNT(*) FROM {table}"))
            .await
            .unwrap_or_else(|e| panic!("{table} missing after migration: {e}"));
    }
    for table in [
        "types_registry__routing_config",
        "types_registry__source_claim",
    ] {
        assert!(
            exec(db, format!("SELECT COUNT(*) FROM {table}"))
                .await
                .is_err(),
            "{table} is federation-only and must not exist in P0"
        );
    }

    exec(
        db,
        format!(
            "INSERT INTO types_registry__version_family \
             (family_key, ownership_scope, owner_tenant_id, created_at) \
             VALUES ('{FAMILY_KEY}', 1, NULL, '{TS}')"
        ),
    )
    .await
    .expect("insert global version family");

    // Identity values are backend-assigned, so read the id back rather than
    // assuming 1.
    let row = db
        .query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM types_registry__version_family".to_owned(),
        ))
        .await
        .expect("read family id")
        .expect("exactly one family row");
    let family_id: i64 = row.try_get_by_index(0).expect("family id column");

    exec(
        db,
        format!(
            "INSERT INTO types_registry__entity \
             (gts_uuid, gts_id, entity_kind, family_id, ownership_scope, owner_tenant_id, \
              owning_gear, lifecycle_status, resource_version, created_at, updated_at) \
             VALUES ({entity_uuid}, '{GTS_TYPE}', 1, {family_id}, 1, NULL, NULL, 1, 1, \
                     '{TS}', '{TS}')"
        ),
    )
    .await
    .expect_err("ck_tr_entity_owner must reject a global entity with no owning_gear");

    exec(
        db,
        format!(
            "INSERT INTO types_registry__entity \
             (gts_uuid, gts_id, entity_kind, family_id, ownership_scope, owner_tenant_id, \
              owning_gear, lifecycle_status, resource_version, created_at, updated_at) \
             VALUES ({entity_uuid}, '{GTS_TYPE}', 1, {family_id}, 1, NULL, 'types-registry', \
                     1, 1, '{TS}', '{TS}')"
        ),
    )
    .await
    .expect("a global entity naming its owning gear is admissible");

    exec(
        db,
        format!(
            "INSERT INTO types_registry__operation \
             (id, kind, dry_run, plane, tenant_id, principal_id, idempotency_key, \
              idempotency_scope_hash, request_fingerprint, status, created_at) \
             VALUES ({op_id}, 1, FALSE, 1, NULL, {principal}, 'idem-1', \
                     {hash}, {hash}, 1, '{TS}')",
            hash = binary_literal(db)
        ),
    )
    .await
    .expect("insert pending registration operation");

    exec(
        db,
        format!(
            "INSERT INTO types_registry__operation_item \
             (operation_id, item_no, gts_id, dry_run, kind, expected_resource_version, status, \
              request_payload, result_revision_no, result_resource_version, error_payload, \
              created_at, started_at, completed_at) \
             VALUES ({op_id}, 0, '{GTS_TYPE}', FALSE, 1, 0, 3, NULL, NULL, 1, NULL, \
                     '{TS}', '{TS}', '{TS}')"
        ),
    )
    .await
    .expect_err("ck_tr_operation_item_state must reject a succeeded registration with no revision");

    exec(
        db,
        format!(
            "INSERT INTO types_registry__operation_item \
             (operation_id, item_no, gts_id, dry_run, kind, expected_resource_version, status, \
              request_payload, result_revision_no, result_resource_version, error_payload, \
              created_at, started_at, completed_at) \
             VALUES ({op_id}, 0, '{GTS_TYPE}', FALSE, 1, 0, 3, NULL, 1, 1, NULL, \
                     '{TS}', '{TS}', '{TS}')"
        ),
    )
    .await
    .expect("a succeeded registration carrying its revision and resource version is admissible");

    // fk_tr_entity_family is RESTRICT, and Postgres/MySQL both enforce it.
    exec(db, "DELETE FROM types_registry__version_family".to_owned())
        .await
        .expect_err("fk_tr_entity_family RESTRICT must block dropping a family with members");
}

/// `uuid` takes different literal syntax as well. Postgres has a native `uuid`
/// type that accepts the 36-character text form; `MySQL` stores the 16 raw bytes
/// `sqlx` binds in `BINARY(16)` and so needs a hex literal. See the migration
/// module's header for why the storage differs.
fn uuid_literal(db: &DatabaseConnection, text: &str) -> String {
    match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => format!("'{text}'"),
        _ => format!("x'{}'", text.replace('-', "")),
    }
}

/// `bytea` and `VARBINARY` take different literal syntax.
fn binary_literal(db: &DatabaseConnection) -> &'static str {
    match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => "'\\x00'::bytea",
        _ => "X'00'",
    }
}

#[tokio::test]
async fn migration_applies_and_rolls_back_on_postgres() {
    use testcontainers_modules::postgres::Postgres;

    let request = ContainerRequest::from(Postgres::default())
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = request.start().await.expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres port");
    wait_for_tcp("127.0.0.1", port, Duration::from_mins(1)).await;

    let db = Database::connect(format!("postgres://user:pass@127.0.0.1:{port}/app"))
        .await
        .expect("connect postgres");

    Migrator::up(&db, None).await.expect("apply on postgres");
    assert_schema_behaves(&db).await;
    Migrator::down(&db, None)
        .await
        .expect("roll back on postgres");
    for table in P0_TABLES {
        assert!(
            exec(&db, format!("SELECT COUNT(*) FROM {table}"))
                .await
                .is_err(),
            "{table} survived `down` on postgres"
        );
    }
    Migrator::up(&db, None).await.expect("re-apply on postgres");
}

#[tokio::test]
async fn migration_applies_and_rolls_back_on_mysql() {
    use testcontainers_modules::mysql::Mysql;

    let container = Mysql::default()
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    wait_for_tcp("127.0.0.1", port, Duration::from_mins(2)).await;

    let db = Database::connect(format!("mysql://root@127.0.0.1:{port}/test"))
        .await
        .expect("connect mysql");

    Migrator::up(&db, None).await.expect("apply on mysql");
    assert_schema_behaves(&db).await;
    Migrator::down(&db, None).await.expect("roll back on mysql");
    for table in P0_TABLES {
        assert!(
            exec(&db, format!("SELECT COUNT(*) FROM {table}"))
                .await
                .is_err(),
            "{table} survived `down` on mysql"
        );
    }
    Migrator::up(&db, None).await.expect("re-apply on mysql");
}

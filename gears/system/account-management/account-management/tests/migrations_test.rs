//! Schema-level tests for the m0006 audit-comments migration.
//!
//! Two contracts are pinned at the SQL layer:
//!
//! 1. **Data survival on `up`.** A `conversion_requests` row inserted
//!    against the pre-m0006 schema (i.e. only m0001..m0005 applied)
//!    keeps every column intact after m0006 lands, with the four new
//!    audit-comment columns surfacing as `NULL`. The promise documented
//!    on `m0006_add_conversion_audit_comments` — null-permissive
//!    `CHECK`, no backfill — would silently regress if a future
//!    backend-specific DDL fragment dropped or rewrote a column without
//!    preserving its value.
//!
//! 2. **`up` / `down` / `up` roundtrip idempotency.** Applying m0006,
//!    rolling it back via the `down` direction, and re-applying it
//!    leaves the audit-comment columns present and writable. Captures
//!    a class of regressions where `down()` half-drops columns (e.g.
//!    forgets one of the four), leaving the table in a state where the
//!    subsequent `up()` would fail with `duplicate column name`.
//!
//! The MySQL `DbErr::Custom(MYSQL_NOT_SUPPORTED)` rejection branch is
//! NOT exercised here. SeaORM's `mock` feature is not enabled in this
//! workspace (see top-level `Cargo.toml`), so we cannot construct a
//! `SchemaManager` reporting `DatabaseBackend::MySql` without a live
//! MySQL container. The behaviour is held by code review and by the
//! gear-level docstring on the m0006 source.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use account_management::Migrator;

const TENANT_ID: &str = "00000000-0000-0000-0000-00000000aaaa";
const REQ_ID: &str = "00000000-0000-0000-0000-00000000bbbb";
const REQUESTER: &str = "00000000-0000-0000-0000-00000000cccc";

async fn fresh_sqlite() -> DatabaseConnection {
    Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite")
}

fn stmt(db: &DatabaseConnection, sql: impl Into<String>) -> Statement {
    Statement::from_string(db.get_database_backend(), sql.into())
}

/// Seed one tenant row and one `Pending` conversion-request row using
/// raw SQL, bypassing the AM repos. The pre-m0006 schema does not
/// know about the audit-comment columns, so we deliberately do NOT
/// touch them — that is the whole point of test (1) below.
async fn seed_tenant_and_pending_conversion(db: &DatabaseConnection) {
    let tenant_type_uuid = Uuid::nil();
    db.execute_raw(stmt(
        db,
        format!(
            "INSERT INTO tenants (id, parent_id, name, status, self_managed, \
             tenant_type_uuid, depth) \
             VALUES ('{TENANT_ID}', NULL, 'root', 0, 0, '{tenant_type_uuid}', 0)"
        ),
    ))
    .await
    .expect("seed tenant");

    db.execute_raw(stmt(
        db,
        format!(
            "INSERT INTO conversion_requests \
             (id, tenant_id, parent_id, child_tenant_name, initiator_side, \
              target_mode, status, requested_by, expires_at) \
             VALUES \
             ('{REQ_ID}', '{TENANT_ID}', NULL, 'root', 1, 1, 0, '{REQUESTER}', \
              '2099-12-31T00:00:00Z')"
        ),
    ))
    .await
    .expect("seed pending conversion");
}

// ────────────────────────────────────────────────────────────────────
// Test 1: data survival on `up`
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn m0006_up_preserves_existing_conversion_rows() {
    let db = fresh_sqlite().await;

    // Apply m0001..m0005. The migration set has six entries today, so
    // `Some(5)` stops one short of m0006. If the migration set grows,
    // this constant MUST be revisited — the test asserts the schema
    // shape that holds *immediately before* m0006 lands.
    Migrator::up(&db, Some(5))
        .await
        .expect("apply m0001..m0005");

    seed_tenant_and_pending_conversion(&db).await;

    // Sanity check: the row is there and the audit-comment columns
    // are NOT yet on the schema. Probing one of them MUST fail.
    let probe = db
        .execute_raw(stmt(
            &db,
            "SELECT requested_comment FROM conversion_requests LIMIT 0",
        ))
        .await;
    assert!(
        probe.is_err(),
        "requested_comment column MUST NOT exist before m0006 — probe returned: {probe:?}"
    );

    // Apply m0006.
    Migrator::up(&db, None).await.expect("apply m0006");

    let row = db
        .query_one_raw(stmt(
            &db,
            format!(
                "SELECT id, tenant_id, child_tenant_name, initiator_side, \
                 target_mode, status, requested_by, requested_comment, \
                 approved_comment, cancelled_comment, rejected_comment \
                 FROM conversion_requests WHERE id = '{REQ_ID}'"
            ),
        ))
        .await
        .expect("post-up query")
        .expect("seeded row MUST survive m0006 up");

    // Every pre-m0006 column carries its seeded value verbatim.
    assert_eq!(row.try_get::<String>("", "id").unwrap(), REQ_ID);
    assert_eq!(row.try_get::<String>("", "tenant_id").unwrap(), TENANT_ID);
    assert_eq!(
        row.try_get::<String>("", "child_tenant_name").unwrap(),
        "root"
    );
    assert_eq!(row.try_get::<i16>("", "initiator_side").unwrap(), 1);
    assert_eq!(row.try_get::<i16>("", "target_mode").unwrap(), 1);
    assert_eq!(row.try_get::<i16>("", "status").unwrap(), 0);
    assert_eq!(
        row.try_get::<String>("", "requested_by").unwrap(),
        REQUESTER
    );

    // All four new columns surface as NULL on the pre-existing row —
    // m0006 promises no backfill.
    assert!(
        row.try_get::<Option<String>>("", "requested_comment")
            .unwrap()
            .is_none(),
        "requested_comment MUST be NULL on a pre-m0006 row"
    );
    assert!(
        row.try_get::<Option<String>>("", "approved_comment")
            .unwrap()
            .is_none(),
        "approved_comment MUST be NULL on a pre-m0006 row"
    );
    assert!(
        row.try_get::<Option<String>>("", "cancelled_comment")
            .unwrap()
            .is_none(),
        "cancelled_comment MUST be NULL on a pre-m0006 row"
    );
    assert!(
        row.try_get::<Option<String>>("", "rejected_comment")
            .unwrap()
            .is_none(),
        "rejected_comment MUST be NULL on a pre-m0006 row"
    );
}

// ────────────────────────────────────────────────────────────────────
// Test 2: up / down / up roundtrip
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn m0006_up_down_up_roundtrip_preserves_audit_columns() {
    let db = fresh_sqlite().await;

    // First up — apply every migration including m0006.
    Migrator::up(&db, None).await.expect("initial up");

    // Audit columns are on the schema.
    db.execute_raw(stmt(
        &db,
        "SELECT requested_comment, approved_comment, cancelled_comment, \
         rejected_comment FROM conversion_requests LIMIT 0",
    ))
    .await
    .expect("audit columns exist after first up");

    // Roll back three steps — m0008 (drop `integrity_check_runs`), m0007
    // (`am_leases`), then m0006 (the audit columns under test). SeaORM's
    // tracker (`seaql_migrations`) is the source of truth for "which is
    // last"; m0007/m0008 landed after m0006 (AM lease port), so the step
    // count was bumped 1 → 3. A further migration after m0008 would shift
    // this again and the test would need updating alongside.
    Migrator::down(&db, Some(3))
        .await
        .expect("down to pre-m0006");

    let probe = db
        .execute_raw(stmt(
            &db,
            "SELECT requested_comment FROM conversion_requests LIMIT 0",
        ))
        .await;
    assert!(
        probe.is_err(),
        "audit columns MUST NOT exist after m0006 down — probe returned: {probe:?}"
    );

    // Re-apply m0006. A half-broken `down()` (e.g. dropping only three
    // of the four columns) would surface here as a `duplicate column
    // name` error on the re-add.
    Migrator::up(&db, None).await.expect("re-apply m0006");

    // Schema is back. Seed a fresh tenant + conversion and write into
    // the audit-comment column to confirm the CHECK constraint
    // accepts an in-range value end-to-end through the round trip.
    seed_tenant_and_pending_conversion(&db).await;
    db.execute_raw(stmt(
        &db,
        format!(
            "UPDATE conversion_requests SET requested_comment = 'why' \
             WHERE id = '{REQ_ID}'"
        ),
    ))
    .await
    .expect("write requested_comment after roundtrip");
    let row = db
        .query_one_raw(stmt(
            &db,
            format!(
                "SELECT requested_comment FROM conversion_requests \
                 WHERE id = '{REQ_ID}'"
            ),
        ))
        .await
        .expect("read")
        .expect("row exists");
    assert_eq!(
        row.try_get::<String>("", "requested_comment").unwrap(),
        "why",
        "audit column MUST round-trip a written value end-to-end after up/down/up"
    );
}

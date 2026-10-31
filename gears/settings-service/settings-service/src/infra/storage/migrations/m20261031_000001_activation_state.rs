// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! Consumer activation's schema (DESIGN-activation §4.7, DESIGN.md §4.7).
//!
//! * `subscriptions` — one row per `(subscriber, key, tenant)`; the unique
//!   `uq_sub_subscriber_key_tenant` makes a registration idempotent and
//!   `idx_sub_key` serves the one read of a key's subscriptions a change makes
//!   under the key's mutation lock.
//! * `activation_state` — at most one row per subscription, the delivery
//!   queue and the expectation a report is verified against. `change_ref`
//!   names the change's audit record **by value, with no foreign key**: audit
//!   records leave by retention while an `awaiting` state has no deadline.
//!   `idx_activation_awaiting` is partial (`WHERE status = 'awaiting'`) on both
//!   backends, as `idx_audit_change_ref` is.
//! * `audit_records` — the `activation` value of `operation`, the nullable
//!   `change_ref` and `detail` columns, and the partial `idx_audit_change_ref`.
//!
//! The `operation` vocabulary is a `CHECK`. `PostgreSQL` replaces the
//! constraint in place: it finds the existing `CHECK` on `operation` through
//! `pg_constraint` rather than assuming its generated name, drops it, and adds
//! the new one `NOT VALID` and then validates it. `SQLite` cannot alter a `CHECK`, so the table is
//! rebuilt: created anew, filled from the old one, the old dropped and the new
//! renamed over it, and the indexes and the append-only trigger of the earlier
//! migrations recreated on it.
//!
//! Rolling back is clean only while no `activation` record exists: the
//! narrowed `CHECK` refuses one, as an earlier build's history read refuses an
//! `operation` it does not know.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The operations an earlier build knows.
const OPERATIONS_BEFORE: &str =
    "'create', 'change', 'revert', 'remove', 'clone', 'secret_use', 'stage'";
/// The operations this build knows.
const OPERATIONS_AFTER: &str =
    "'create', 'change', 'revert', 'remove', 'clone', 'secret_use', 'stage', 'activation'";

/// Drops every `CHECK` of `audit_records` that constrains `operation`, found
/// through `pg_constraint` by table and definition: the name an earlier
/// migration's constraint was given is not assumed.
const DROP_OPERATION_CHECKS: &str = r"DO $$
DECLARE
    found_name text;
BEGIN
    FOR found_name IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'audit_records'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) ~ '\yoperation\y'
    LOOP
        EXECUTE format('ALTER TABLE audit_records DROP CONSTRAINT %I', found_name);
    END LOOP;
END
$$;";

/// The columns both shapes of `audit_records` share.
const AUDIT_COLUMNS: &str = "id, resource, declaration_key, tenant_id, operation, actor, \
     actor_classification, pre_value, post_value, outcome, request_id, change_set_id, \
     occurred_at, retain_until";

/// The `SQLite` shape of `audit_records` with the given operation vocabulary,
/// and the two additive columns when `activation` is set.
fn sqlite_audit_table(name: &str, operations: &str, activation: bool) -> String {
    let extra = if activation {
        ",
                    change_ref            text,
                    detail                text"
    } else {
        ""
    };
    format!(
        "CREATE TABLE {name} (
                    id                    text     PRIMARY KEY,
                    resource              text     NOT NULL,
                    declaration_key       text     NOT NULL,
                    tenant_id             text     NULL,
                    operation             text     NOT NULL
                                          CHECK (operation IN ({operations})),
                    actor                 text     NOT NULL,
                    actor_classification  text     NOT NULL
                                          CHECK (actor_classification IN ('public', 'pii')),
                    pre_value             text,
                    post_value            text,
                    outcome               text     NOT NULL
                                          CHECK (outcome IN ('success', 'failure')),
                    request_id            text     NOT NULL,
                    change_set_id         text,
                    occurred_at           text     NOT NULL,
                    retain_until          text{extra}
                );"
    )
}

/// What an earlier migration put on `audit_records` besides its columns, which
/// a rebuild on `SQLite` drops with the table.
const SQLITE_AUDIT_FIXTURES: [&str; 4] = [
    "CREATE INDEX IF NOT EXISTS idx_audit_scoped
         ON audit_records (declaration_key, tenant_id, occurred_at DESC);",
    "CREATE INDEX IF NOT EXISTS idx_audit_retention
         ON audit_records (retain_until)
         WHERE retain_until IS NOT NULL;",
    "CREATE INDEX IF NOT EXISTS idx_audit_default_horizon
         ON audit_records (occurred_at)
         WHERE retain_until IS NULL;",
    r"CREATE TRIGGER IF NOT EXISTS trg_audit_records_append_only
        BEFORE UPDATE ON audit_records
    BEGIN
        SELECT RAISE(ABORT, 'audit_records is append-only: UPDATE is not permitted');
    END;",
];

async fn run(conn: &impl ConnectionTrait, statements: &[String]) -> Result<(), DbErr> {
    for sql in statements {
        conn.execute_unprepared(sql).await?;
    }
    Ok(())
}

/// Rebuild `audit_records` on `SQLite` with another shape.
async fn rebuild_sqlite_audit(
    conn: &impl ConnectionTrait,
    operations: &str,
    activation: bool,
) -> Result<(), DbErr> {
    let mut statements = vec![
        "DROP TABLE IF EXISTS audit_records_rebuilt;".to_owned(),
        sqlite_audit_table("audit_records_rebuilt", operations, activation),
        format!(
            "INSERT INTO audit_records_rebuilt ({AUDIT_COLUMNS})
                 SELECT {AUDIT_COLUMNS} FROM audit_records;"
        ),
        "DROP TABLE audit_records;".to_owned(),
        "ALTER TABLE audit_records_rebuilt RENAME TO audit_records;".to_owned(),
    ];
    statements.extend(SQLITE_AUDIT_FIXTURES.iter().map(|s| (*s).to_owned()));
    run(conn, &statements).await
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            let statements: Vec<String> = vec![
                r"CREATE TABLE IF NOT EXISTS subscriptions (
                    id          uuid         PRIMARY KEY,
                    subscriber  text         NOT NULL,
                    key         text         NOT NULL,
                    tenant      text         NOT NULL,
                    created_at  timestamptz  NOT NULL DEFAULT now(),
                    CONSTRAINT uq_sub_subscriber_key_tenant UNIQUE (subscriber, key, tenant)
                );"
                .to_owned(),
                "CREATE INDEX IF NOT EXISTS idx_sub_key ON subscriptions (key);".to_owned(),
                r"CREATE TABLE IF NOT EXISTS activation_state (
                    subscriber        text         NOT NULL,
                    key               text         NOT NULL,
                    tenant            text         NOT NULL,
                    change_ref        uuid         NOT NULL,
                    expected_value    text         NOT NULL,
                    status            text         NOT NULL DEFAULT 'awaiting'
                                      CHECK (status IN ('awaiting', 'succeeded', 'failed')),
                    applied_value     text,
                    detail            text,
                    reported_at       timestamptz,
                    last_notified_at  timestamptz,
                    updated_at        timestamptz  NOT NULL DEFAULT now(),
                    CONSTRAINT uq_activation_state UNIQUE (subscriber, key, tenant)
                );"
                .to_owned(),
                "CREATE INDEX IF NOT EXISTS idx_activation_key_tenant
                     ON activation_state (key, tenant);"
                    .to_owned(),
                "CREATE INDEX IF NOT EXISTS idx_activation_awaiting
                     ON activation_state (subscriber, tenant)
                     WHERE status = 'awaiting';"
                    .to_owned(),
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS change_ref uuid;".to_owned(),
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS detail text;".to_owned(),
                DROP_OPERATION_CHECKS.to_owned(),
                format!(
                    "ALTER TABLE audit_records ADD CONSTRAINT audit_records_operation_check
                         CHECK (operation IN ({OPERATIONS_AFTER})) NOT VALID;"
                ),
                "ALTER TABLE audit_records VALIDATE CONSTRAINT audit_records_operation_check;"
                    .to_owned(),
                "CREATE INDEX IF NOT EXISTS idx_audit_change_ref
                     ON audit_records (change_ref)
                     WHERE change_ref IS NOT NULL;"
                    .to_owned(),
            ];
            return run(conn, &statements).await;
        }
        let statements: Vec<String> = vec![
            r"CREATE TABLE IF NOT EXISTS subscriptions (
                    id          text  PRIMARY KEY,
                    subscriber  text  NOT NULL,
                    key         text  NOT NULL,
                    tenant      text  NOT NULL,
                    created_at  text  NOT NULL,
                    CONSTRAINT uq_sub_subscriber_key_tenant UNIQUE (subscriber, key, tenant)
                );"
            .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_sub_key ON subscriptions (key);".to_owned(),
            r"CREATE TABLE IF NOT EXISTS activation_state (
                    subscriber        text  NOT NULL,
                    key               text  NOT NULL,
                    tenant            text  NOT NULL,
                    change_ref        text  NOT NULL,
                    expected_value    text  NOT NULL,
                    status            text  NOT NULL DEFAULT 'awaiting'
                                      CHECK (status IN ('awaiting', 'succeeded', 'failed')),
                    applied_value     text,
                    detail            text,
                    reported_at       text,
                    last_notified_at  text,
                    updated_at        text  NOT NULL,
                    CONSTRAINT uq_activation_state UNIQUE (subscriber, key, tenant)
                );"
            .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_activation_key_tenant
                 ON activation_state (key, tenant);"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_activation_awaiting
                 ON activation_state (subscriber, tenant)
                 WHERE status = 'awaiting';"
                .to_owned(),
        ];
        run(conn, &statements).await?;
        rebuild_sqlite_audit(conn, OPERATIONS_AFTER, true).await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_audit_change_ref
                 ON audit_records (change_ref)
                 WHERE change_ref IS NOT NULL;",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            let statements: Vec<String> = vec![
                "DROP INDEX IF EXISTS idx_audit_change_ref;".to_owned(),
                DROP_OPERATION_CHECKS.to_owned(),
                format!(
                    "ALTER TABLE audit_records ADD CONSTRAINT audit_records_operation_check
                         CHECK (operation IN ({OPERATIONS_BEFORE})) NOT VALID;"
                ),
                "ALTER TABLE audit_records VALIDATE CONSTRAINT audit_records_operation_check;"
                    .to_owned(),
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS detail;".to_owned(),
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS change_ref;".to_owned(),
                "DROP TABLE IF EXISTS activation_state;".to_owned(),
                "DROP TABLE IF EXISTS subscriptions;".to_owned(),
            ];
            return run(conn, &statements).await;
        }
        conn.execute_unprepared("DROP INDEX IF EXISTS idx_audit_change_ref;")
            .await?;
        rebuild_sqlite_audit_down(conn).await?;
        run(
            conn,
            &[
                "DROP TABLE IF EXISTS activation_state;".to_owned(),
                "DROP TABLE IF EXISTS subscriptions;".to_owned(),
            ],
        )
        .await
    }
}

/// The inverse rebuild: the earlier vocabulary and no additive columns. The
/// copy names the shared columns only, so the two additive ones are dropped.
async fn rebuild_sqlite_audit_down(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
    rebuild_sqlite_audit(conn, OPERATIONS_BEFORE, false).await
}

#[cfg(test)]
#[path = "m20261031_000001_activation_state_tests.rs"]
mod m20261031_000001_activation_state_tests;

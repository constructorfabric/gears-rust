// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! Consumer activation's schema (DESIGN-activation §4.7, DESIGN.md §4.7), per
//! consumer instance (ADR-004).
//!
//! * `subscriptions` — one row per `(subscriber, instance_id, key, tenant)`:
//!   each process of a consuming gear registers for itself. The unique
//!   `uq_sub_subscriber_instance_key_tenant` makes a repeat registration of an
//!   instance idempotent; `idx_sub_key` serves the one read of a key's
//!   subscriptions a change makes under the key's mutation lock and the
//!   registration cap's count and eviction; the partial `idx_sub_silent` on
//!   `(key, tenant, silent_since)` over the rows whose `silent_since` is set
//!   serves the excluded entries of the activation read and the forgetting.
//!   `instance_label` is informational, at most 128 characters.
//! * `activation_state` — at most one row per subscription, the delivery
//!   queue and the expectation a report is verified against. `change_ref`
//!   names the change's audit record **by value, with no foreign key**: audit
//!   records leave by retention while a state lives until its instance
//!   reports, a later change replaces it, or its subscription goes.
//!   `changed_at` has no default: the change's insert-select sets it to the
//!   database's `now()`. `deliveries` counts the accepted publications of the
//!   current `change_ref`; `status` takes `silent` beside `awaiting`,
//!   `succeeded` and `failed`. `idx_activation_awaiting` is partial
//!   (`WHERE status = 'awaiting'`) on both backends, as `idx_sub_silent` is.
//! * `audit_records` — the `activation` value of `operation`; the nullable
//!   `change_ref`, `detail`, `instance_id` and `instance_label` columns, the
//!   last two set on `activation` records only; and the `outcome` check
//!   widened by `silent` on an `activation` record only. The partial
//!   `idx_audit_change_ref` on the new `change_ref` is the next migration's,
//!   `m20261031_000002_audit_change_ref_index`, for the lock reason below.
//!
//! This is the feature's own, unreleased migration: the per-instance columns
//! and the widened checks are part of it rather than of a new one, so no
//! existing row is converted.
//!
//! The `operation` vocabulary and the `outcome` rule are `CHECK`s.
//! `PostgreSQL` replaces them in place: it finds the existing `CHECK`s on
//! `outcome` and on `operation` through `pg_constraint` rather than assuming
//! their generated names, drops them, and adds the wider ones `NOT VALID`,
//! and does **not** validate them. `NOT VALID` still enforces the check on
//! every row inserted or updated from then on; it only skips the scan of the
//! rows already there, and that scan could not reject any of them: the wider
//! checks only add values (`activation`; `silent` on an `activation` record)
//! to rules every existing row already satisfies, validated since
//! `m20260907_000001_audit_records` created them. The constraints stay
//! `convalidated = false` in `pg_constraint`, which nothing here reads.
//! `SQLite` cannot alter a `CHECK`, so the table is rebuilt: created anew,
//! filled from the old one, the old dropped and the new renamed over it, and
//! the indexes and the append-only trigger of the earlier migrations recreated
//! on it.
//!
//! **The lock window on `PostgreSQL`.** The toolkit runner applies `up()` and
//! the history record in one transaction per migration, so the
//! `ACCESS EXCLUSIVE` lock the first `ALTER TABLE audit_records` takes is held
//! to this migration's commit, blocking every reader and writer of
//! `audit_records` — every settings write records into it — for as long as
//! the transaction lasts. Everything this migration does to `audit_records`
//! is catalog-only: four nullable columns with no default (no table rewrite),
//! two `CHECK`s dropped, two added `NOT VALID`; the two new tables and their
//! indexes are created empty. The lock is therefore held for a time
//! independent of the table's size. It must still be granted: it waits behind
//! any open transaction on `audit_records`, and queues every later one behind
//! itself while it waits. Validating the checks here would have held it across
//! two full scans, and building `idx_audit_change_ref` here across a third;
//! the index is built in the next migration's own transaction, under the
//! `SHARE` lock `CREATE INDEX` takes alone.
//!
//! Rolling back is clean only while no `activation` record exists: the
//! narrowed `CHECK`s refuse one, as an earlier build's history read refuses an
//! `operation` or an `outcome` it does not know. `down()` therefore validates
//! the narrowed checks — there the scan is the guard, since a narrower rule
//! can reject an existing row — and holds `ACCESS EXCLUSIVE` across it; a
//! rollback is an operator's deliberate step, where that is accepted.

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

/// The outcome rule an earlier build knows.
const OUTCOMES_BEFORE: &str = "outcome IN ('success', 'failure')";
/// The outcome rule this build knows: `silent` — the record of a call of
/// silence — on an `activation` record only.
const OUTCOMES_AFTER: &str =
    "outcome IN ('success', 'failure') OR (operation = 'activation' AND outcome = 'silent')";

/// Drops every `CHECK` of `audit_records` whose definition names `column`,
/// found through `pg_constraint` by table and definition: the name an earlier
/// migration's constraint was given is not assumed.
fn drop_checks_on(column: &str) -> String {
    format!(
        r"DO $$
DECLARE
    found_name text;
BEGIN
    FOR found_name IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'audit_records'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) ~ '\y{column}\y'
    LOOP
        EXECUTE format('ALTER TABLE audit_records DROP CONSTRAINT %I', found_name);
    END LOOP;
END
$$;"
    )
}

/// Whether [`pg_audit_checks`] scans the existing rows against the new checks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingRows {
    /// Widening: no existing row can fail a rule that only adds values, so
    /// the checks stay `NOT VALID` and the table is not scanned under the
    /// lock (see the module comment).
    Trusted,
    /// Narrowing: an existing row can fail, so the checks are validated and a
    /// row that fails refuses the step.
    Validated,
}

/// The `PostgreSQL` statements that put the `operation` and `outcome` checks
/// of `audit_records` in place with the given rules. The outcome check is
/// dropped first: once widened it names `operation` too, so dropping by
/// `operation` first would also catch it; dropping both before adding either
/// keeps each drop exact.
fn pg_audit_checks(operations: &str, outcomes: &str, existing: ExistingRows) -> Vec<String> {
    let mut statements = vec![
        drop_checks_on("outcome"),
        drop_checks_on("operation"),
        format!(
            "ALTER TABLE audit_records ADD CONSTRAINT audit_records_operation_check
                 CHECK (operation IN ({operations})) NOT VALID;"
        ),
        format!(
            "ALTER TABLE audit_records ADD CONSTRAINT audit_records_outcome_check
                 CHECK ({outcomes}) NOT VALID;"
        ),
    ];
    if existing == ExistingRows::Validated {
        statements.extend([
            "ALTER TABLE audit_records VALIDATE CONSTRAINT audit_records_operation_check;"
                .to_owned(),
            "ALTER TABLE audit_records VALIDATE CONSTRAINT audit_records_outcome_check;".to_owned(),
        ]);
    }
    statements
}

/// The columns both shapes of `audit_records` share — `actor_tenant_id` is
/// the setter-tenant migration's, which runs before this one.
const AUDIT_COLUMNS: &str = "id, resource, declaration_key, tenant_id, operation, actor, \
     actor_classification, pre_value, post_value, outcome, request_id, change_set_id, \
     occurred_at, retain_until, actor_tenant_id";

/// The `SQLite` shape of `audit_records`: with `activation` set, this build's
/// vocabulary, the widened outcome rule and the four additive columns;
/// without it, an earlier build's.
fn sqlite_audit_table(name: &str, activation: bool) -> String {
    let (operations, outcomes, extra) = if activation {
        (
            OPERATIONS_AFTER,
            OUTCOMES_AFTER,
            ",
                    change_ref            text,
                    detail                text,
                    instance_id           text,
                    instance_label        text",
        )
    } else {
        (OPERATIONS_BEFORE, OUTCOMES_BEFORE, "")
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
                    outcome               text     NOT NULL,
                    request_id            text     NOT NULL,
                    change_set_id         text,
                    occurred_at           text     NOT NULL,
                    retain_until          text,
                    actor_tenant_id       text{extra},
                    CONSTRAINT audit_records_outcome_check CHECK ({outcomes})
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

/// Rebuild `audit_records` on `SQLite` with this build's shape (`activation`)
/// or an earlier build's. The copy names the shared columns only, so a
/// rebuild back to the earlier shape drops the additive ones.
async fn rebuild_sqlite_audit(conn: &impl ConnectionTrait, activation: bool) -> Result<(), DbErr> {
    let mut statements = vec![
        "DROP TABLE IF EXISTS audit_records_rebuilt;".to_owned(),
        sqlite_audit_table("audit_records_rebuilt", activation),
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

/// The index statements both backends share: the key's fan-out and cap
/// lookups, the silent subscriptions, the activation read and the delivery
/// scan. Partial indexes are written the same way on both.
const SHARED_INDEXES: [&str; 4] = [
    "CREATE INDEX IF NOT EXISTS idx_sub_key ON subscriptions (key);",
    "CREATE INDEX IF NOT EXISTS idx_sub_silent
         ON subscriptions (key, tenant, silent_since)
         WHERE silent_since IS NOT NULL;",
    "CREATE INDEX IF NOT EXISTS idx_activation_key_tenant
         ON activation_state (key, tenant);",
    "CREATE INDEX IF NOT EXISTS idx_activation_awaiting
         ON activation_state (subscriber, tenant)
         WHERE status = 'awaiting';",
];

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            let mut statements: Vec<String> = vec![
                r"CREATE TABLE IF NOT EXISTS subscriptions (
                    id              uuid         PRIMARY KEY,
                    subscriber      text         NOT NULL,
                    instance_id     uuid         NOT NULL,
                    instance_label  text
                                    CHECK (char_length(instance_label) <= 128),
                    key             text         NOT NULL,
                    tenant          text         NOT NULL,
                    silent_since    timestamptz,
                    created_at      timestamptz  NOT NULL DEFAULT now(),
                    CONSTRAINT uq_sub_subscriber_instance_key_tenant
                        UNIQUE (subscriber, instance_id, key, tenant)
                );"
                .to_owned(),
                r"CREATE TABLE IF NOT EXISTS activation_state (
                    subscriber        text         NOT NULL,
                    instance_id       uuid         NOT NULL,
                    key               text         NOT NULL,
                    tenant            text         NOT NULL,
                    change_ref        uuid         NOT NULL,
                    changed_at        timestamptz  NOT NULL,
                    expected_value    text         NOT NULL,
                    status            text         NOT NULL DEFAULT 'awaiting'
                                      CHECK (status IN ('awaiting', 'succeeded', 'failed',
                                                        'silent')),
                    deliveries        integer      NOT NULL DEFAULT 0,
                    silent_since      timestamptz,
                    applied_value     text,
                    detail            text,
                    reported_at       timestamptz,
                    last_notified_at  timestamptz,
                    updated_at        timestamptz  NOT NULL DEFAULT now(),
                    CONSTRAINT uq_activation_state
                        UNIQUE (subscriber, instance_id, key, tenant)
                );"
                .to_owned(),
            ];
            statements.extend(SHARED_INDEXES.iter().map(|s| (*s).to_owned()));
            statements.extend([
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS change_ref uuid;".to_owned(),
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS detail text;".to_owned(),
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS instance_id uuid;".to_owned(),
                "ALTER TABLE audit_records ADD COLUMN IF NOT EXISTS instance_label text;"
                    .to_owned(),
            ]);
            statements.extend(pg_audit_checks(
                OPERATIONS_AFTER,
                OUTCOMES_AFTER,
                ExistingRows::Trusted,
            ));
            return run(conn, &statements).await;
        }
        let mut statements: Vec<String> = vec![
            r"CREATE TABLE IF NOT EXISTS subscriptions (
                    id              text  PRIMARY KEY,
                    subscriber      text  NOT NULL,
                    instance_id     text  NOT NULL,
                    instance_label  text
                                    CHECK (length(instance_label) <= 128),
                    key             text  NOT NULL,
                    tenant          text  NOT NULL,
                    silent_since    text,
                    created_at      text  NOT NULL,
                    CONSTRAINT uq_sub_subscriber_instance_key_tenant
                        UNIQUE (subscriber, instance_id, key, tenant)
                );"
            .to_owned(),
            r"CREATE TABLE IF NOT EXISTS activation_state (
                    subscriber        text     NOT NULL,
                    instance_id       text     NOT NULL,
                    key               text     NOT NULL,
                    tenant            text     NOT NULL,
                    change_ref        text     NOT NULL,
                    changed_at        text     NOT NULL,
                    expected_value    text     NOT NULL,
                    status            text     NOT NULL DEFAULT 'awaiting'
                                      CHECK (status IN ('awaiting', 'succeeded', 'failed',
                                                        'silent')),
                    deliveries        integer  NOT NULL DEFAULT 0,
                    silent_since      text,
                    applied_value     text,
                    detail            text,
                    reported_at       text,
                    last_notified_at  text,
                    updated_at        text     NOT NULL,
                    CONSTRAINT uq_activation_state
                        UNIQUE (subscriber, instance_id, key, tenant)
                );"
            .to_owned(),
        ];
        statements.extend(SHARED_INDEXES.iter().map(|s| (*s).to_owned()));
        run(conn, &statements).await?;
        rebuild_sqlite_audit(conn, true).await
    }

    /// Runs after the next migration's `down()` has dropped
    /// `idx_audit_change_ref`; dropping `change_ref` on `PostgreSQL`, or the
    /// table rebuild on `SQLite`, would take it with them anyway.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if manager.get_database_backend() == DatabaseBackend::Postgres {
            let mut statements =
                pg_audit_checks(OPERATIONS_BEFORE, OUTCOMES_BEFORE, ExistingRows::Validated);
            statements.extend([
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS instance_label;".to_owned(),
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS instance_id;".to_owned(),
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS detail;".to_owned(),
                "ALTER TABLE audit_records DROP COLUMN IF EXISTS change_ref;".to_owned(),
                "DROP TABLE IF EXISTS activation_state;".to_owned(),
                "DROP TABLE IF EXISTS subscriptions;".to_owned(),
            ]);
            return run(conn, &statements).await;
        }
        rebuild_sqlite_audit(conn, false).await?;
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

#[cfg(test)]
#[path = "m20261031_000001_activation_state_tests.rs"]
mod m20261031_000001_activation_state_tests;

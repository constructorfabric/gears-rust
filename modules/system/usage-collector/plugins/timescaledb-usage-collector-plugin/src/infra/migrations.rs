//! Schema migration runner for the `TimescaleDB` storage plugin.

use sqlx::PgPool;

use crate::domain::error::MigrationError;
use crate::infra::db_error::DbError;

// @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1
/// # Errors
///
/// Returns [`MigrationError`] if any DDL statement fails against the database.
///
/// All migration statements execute inside a single transaction so a mid-sequence
/// failure rolls back to the pre-migration schema. Without the transaction, an
/// `IF NOT EXISTS` re-run can still leave the database in a partially-migrated
/// state — for example with the hypertable created but later indexes missing —
/// and a subsequent retry must reach the failing step before its IF NOT EXISTS
/// short-circuits do any work. `PostgreSQL` and `TimescaleDB` both support DDL
/// inside transactions for every operation used here (`CREATE EXTENSION`,
/// `CREATE TABLE`, `create_hypertable()`, `CREATE INDEX`, `ALTER TABLE`).
pub async fn run_migrations(pool: &PgPool) -> Result<(), MigrationError> {
    // Local helper: build a Migration error tagged with the failing statement.
    // Keeping it inside the function (rather than associated/free) avoids
    // exposing a stringly-typed migration builder in the public error surface.
    fn migration_err(context: &str) -> impl Fn(sqlx::Error) -> MigrationError + '_ {
        move |source| MigrationError::Migration {
            context: context.to_owned(),
            source: DbError::boxed(source),
        }
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(migration_err("failed to begin migration transaction"))?;

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-1
    sqlx::query("CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE")
        .execute(&mut *tx)
        .await
        .map_err(migration_err("failed to create timescaledb extension"))?;

    // `gen_random_uuid()` is built into PostgreSQL 13+, but on PG12 and older it
    // lives in `pgcrypto`. Ensure the extension is present so the
    // `usage_records.id` default works on both — no-op on PG13+ where the
    // function is core.
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pgcrypto")
        .execute(&mut *tx)
        .await
        .map_err(migration_err("failed to create pgcrypto extension"))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-1

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-2
    // The `chk_subject_id_present_when_type` constraint mirrors the SDK's
    // `Subject` invariant (id required, type optional): a row with
    // `subject_type IS NOT NULL AND subject_id IS NULL` cannot be reconstructed
    // into a `Subject` on read, so the read path would silently drop the type.
    // Enforcing it at write time means a future external writer that bypasses
    // the insert port fails loudly instead of producing reads with missing
    // authorization context.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS usage_records (
            id              UUID        NOT NULL DEFAULT gen_random_uuid(),
            tenant_id       UUID        NOT NULL,
            module          TEXT        NOT NULL,
            kind            TEXT        NOT NULL CHECK (kind IN ('counter', 'gauge')),
            metric          TEXT        NOT NULL,
            value           NUMERIC     NOT NULL,
            timestamp       TIMESTAMPTZ NOT NULL,
            idempotency_key TEXT,
            resource_id     UUID        NOT NULL,
            resource_type   TEXT        NOT NULL,
            subject_id      UUID,
            subject_type    TEXT,
            ingested_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            metadata        JSONB,
            PRIMARY KEY (id, timestamp),
            CONSTRAINT chk_subject_id_present_when_type
                CHECK (subject_type IS NULL OR subject_id IS NOT NULL)
        )",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err("failed to create usage_records table"))?;

    // Idempotent attach for pre-existing tables: the `CREATE TABLE IF NOT EXISTS`
    // above is a no-op when the table already exists, so a database created by
    // a prior plugin version would not pick up the constraint without this.
    sqlx::query(
        "DO $$
        BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conname = 'chk_subject_id_present_when_type'
            ) THEN
                ALTER TABLE usage_records
                ADD CONSTRAINT chk_subject_id_present_when_type
                CHECK (subject_type IS NULL OR subject_id IS NOT NULL);
            END IF;
        END;
        $$",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to attach chk_subject_id_present_when_type",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-2

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-3
    sqlx::query("SELECT create_hypertable('usage_records', 'timestamp', if_not_exists => true)")
        .execute(&mut *tx)
        .await
        .map_err(migration_err(
            "failed to convert usage_records to hypertable",
        ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-3

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-4
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_usage_records_tenant_time \
         ON usage_records (tenant_id, timestamp DESC)",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create idx_usage_records_tenant_time",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-4

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-5
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_usage_records_tenant_metric_time \
         ON usage_records (tenant_id, metric, timestamp DESC)",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create idx_usage_records_tenant_metric_time",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-5

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-6
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_usage_records_tenant_subject_time \
         ON usage_records (tenant_id, subject_id, timestamp DESC)",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create idx_usage_records_tenant_subject_time",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-6

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-7
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_usage_records_tenant_resource_time \
         ON usage_records (tenant_id, resource_id, timestamp DESC)",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create idx_usage_records_tenant_resource_time",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-7

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-8
    // Separate plain table for idempotency deduplication. TimescaleDB requires all
    // unique indexes on a hypertable to include the partition column (timestamp), so
    // cross-partition idempotency cannot be enforced with a partial index on usage_records.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS usage_idempotency_keys (
            tenant_id       UUID NOT NULL,
            idempotency_key TEXT NOT NULL,
            PRIMARY KEY (tenant_id, idempotency_key)
        )",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create usage_idempotency_keys table",
    ))?;
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-8

    // Add created_at to idempotency_keys for bounded cleanup; safe to run on existing tables.
    sqlx::query(
        "ALTER TABLE usage_idempotency_keys \
         ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to add created_at to usage_idempotency_keys",
    ))?;

    // Index on created_at so the periodic cleanup in `cleanup_idempotency_keys`
    // can drive its `DELETE ... WHERE created_at < NOW() - interval` predicate
    // off an index rather than a full-table scan. Without it the hourly cleanup
    // grows with table size and competes with ingest on a busy database.
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_usage_idempotency_keys_created_at \
         ON usage_idempotency_keys (created_at)",
    )
    .execute(&mut *tx)
    .await
    .map_err(migration_err(
        "failed to create idx_usage_idempotency_keys_created_at",
    ))?;

    tx.commit()
        .await
        .map_err(migration_err("failed to commit migration transaction"))?;

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-9
    Ok(())
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-schema-migrations:p1:inst-mig-9
}

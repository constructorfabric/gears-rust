//! Immutable value versions (ADR-0006): `credstore_secrets` gains the
//! `value_version` pointer (`TEXT NULL`, `NULL` = `declared`) and the
//! `fallback` column, the status `CHECK` narrows to `(2, 4)`
//! (`active`/`declared`; `1`/`3` retired and reserved), the pointer/status
//! `CHECK` `credstore_secrets_value_version_check` is added, the
//! value-fingerprint fence columns (`value_fp`, `fp_key_id`) and their
//! `CHECK` are dropped, and the reaper's `idx_credstore_pending` is dropped.
//! There is no gc table: nothing in the schema tracks garbage.
//!
//! Per-backend raw SQL, like `m0001_initial_schema`. `PostgreSQL` rewrites the
//! shipped anonymous status `CHECK` in place - its auto-generated name is
//! looked up from `pg_constraint` by the exact column it references, never
//! guessed. `SQLite` cannot `DROP CONSTRAINT` or widen an existing `CHECK`,
//! so it rebuilds the table: `CREATE credstore_secrets_new` with the final
//! schema, `INSERT ... SELECT` only the `active` rows, `DROP` the old table,
//! rename the new one in, and recreate every index. A carried `active` row
//! cannot keep pointing at a backend value: the store key shape changes from
//! `tenant/reference/class` to `(tenant_id, record_id)` and nothing was ever
//! written under it, so every carried row becomes `declared` (`status = 4`,
//! `value_version` `NULL`). `MySQL` is not supported; this migration fails
//! fast with the same error text as `m0001`.

use credstore_sdk::types::GENERIC_TYPE_UUID_STR;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "credstore migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite)";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        match backend {
            sea_orm::DatabaseBackend::Postgres => Self::up_postgres(conn).await,
            sea_orm::DatabaseBackend::Sqlite => Self::up_sqlite(conn).await,
            _ => Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned())),
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        match backend {
            sea_orm::DatabaseBackend::Postgres => Self::down_postgres(conn).await,
            sea_orm::DatabaseBackend::Sqlite => Self::down_sqlite(conn).await,
            _ => Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned())),
        }
    }
}

impl Migration {
    async fn up_postgres(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
        let statements = [
            // Additive columns, idempotent.
            "ALTER TABLE credstore_secrets ADD COLUMN IF NOT EXISTS value_version TEXT NULL;",
            "ALTER TABLE credstore_secrets \
                ADD COLUMN IF NOT EXISTS fallback SMALLINT NOT NULL DEFAULT 1;",
            // The fallback CHECK is added separately (a bare ADD COLUMN ...
            // CHECK is not IF-NOT-EXISTS-safe on its own) - guarded below.
            r"
DO $do$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'credstore_secrets'::regclass
          AND conname = 'ck_credstore_fallback'
    ) THEN
        ALTER TABLE credstore_secrets
            ADD CONSTRAINT ck_credstore_fallback CHECK (fallback IN (1, 2));
    END IF;
END
$do$;
            ",
            "CREATE INDEX IF NOT EXISTS idx_credstore_type \
                ON credstore_secrets (tenant_id, secret_type_uuid);",
            // Same row policy as the SQLite rebuild below, applied in place so
            // the new CHECKs can be added on a table that may not be empty:
            // saga rows (1/3) are dropped, `active` rows are demoted to
            // `declared` because no pre-migration value exists under the new
            // key shape (an external one-off script carries them, DESIGN
            // section 8). A greenfield deploy has no rows.
            "DELETE FROM credstore_secrets WHERE status IN (1, 3);",
            "UPDATE credstore_secrets SET status = 4 WHERE status = 2;",
            // Rewrite the shipped anonymous status CHECK in place. Its
            // auto-generated name is never guessed: it is located by the
            // exact column it references via pg_constraint/pg_attribute.
            r"
DO $do$
DECLARE
    r RECORD;
    status_attnum smallint;
BEGIN
    SELECT attnum INTO status_attnum FROM pg_attribute
        WHERE attrelid = 'credstore_secrets'::regclass AND attname = 'status';

    -- Drop the shipped `CHECK (status IN (1,2,3))`, identified by
    -- referencing exactly the `status` column (never by a guessed name).
    FOR r IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'credstore_secrets'::regclass
          AND contype = 'c'
          AND conkey = ARRAY[status_attnum]
    LOOP
        EXECUTE format('ALTER TABLE credstore_secrets DROP CONSTRAINT %I', r.conname);
    END LOOP;

    ALTER TABLE credstore_secrets
        ADD CONSTRAINT credstore_secrets_status_check CHECK (status IN (2, 4));

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'credstore_secrets'::regclass
          AND conname = 'credstore_secrets_value_version_check'
    ) THEN
        ALTER TABLE credstore_secrets
            ADD CONSTRAINT credstore_secrets_value_version_check
            CHECK ((value_version IS NULL) = (status = 4));
    END IF;
END
$do$;
            ",
            // The value-fingerprint fence is withdrawn (ADR-0003): dropping
            // the columns drops their `(value_fp IS NULL) = (fp_key_id IS
            // NULL)` CHECK with them.
            "ALTER TABLE credstore_secrets DROP COLUMN IF EXISTS value_fp;",
            "ALTER TABLE credstore_secrets DROP COLUMN IF EXISTS fp_key_id;",
            // The reaper's sweep index has nothing left to sweep.
            "DROP INDEX IF EXISTS idx_credstore_pending;",
        ];
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down_postgres(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
        let statements = [
            "CREATE INDEX IF NOT EXISTS idx_credstore_pending \
                ON credstore_secrets (updated_at) WHERE status <> 2;",
            "DROP INDEX IF EXISTS idx_credstore_type;",
            "ALTER TABLE credstore_secrets ADD COLUMN IF NOT EXISTS value_fp BYTEA NULL;",
            "ALTER TABLE credstore_secrets ADD COLUMN IF NOT EXISTS fp_key_id SMALLINT NULL;",
            "ALTER TABLE credstore_secrets DROP CONSTRAINT IF EXISTS credstore_secrets_value_version_check;",
            "ALTER TABLE credstore_secrets DROP CONSTRAINT IF EXISTS credstore_secrets_status_check;",
            "ALTER TABLE credstore_secrets \
                ADD CONSTRAINT credstore_secrets_status_check CHECK (status IN (1, 2, 3));",
            "ALTER TABLE credstore_secrets \
                ADD CONSTRAINT credstore_secrets_fp_check CHECK ((value_fp IS NULL) = (fp_key_id IS NULL));",
            "ALTER TABLE credstore_secrets DROP COLUMN IF EXISTS fallback;",
            "ALTER TABLE credstore_secrets DROP COLUMN IF EXISTS value_version;",
        ];
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn up_sqlite(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
        let generic_uuid_hex = GENERIC_TYPE_UUID_STR.replace('-', "");
        let statements = [
            format!(
                r"
CREATE TABLE credstore_secrets_new (
    id BLOB PRIMARY KEY NOT NULL,
    tenant_id BLOB NOT NULL,
    reference TEXT NOT NULL CHECK (length(reference) BETWEEN 1 AND 255),
    sharing SMALLINT NOT NULL CHECK (sharing IN (1, 2, 3)),
    owner_id BLOB NOT NULL,
    status SMALLINT NOT NULL CHECK (status IN (2, 4)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 1,
    secret_type_uuid BLOB NOT NULL DEFAULT (x'{generic_uuid_hex}'),
    expires_at TEXT NULL,
    value_version TEXT NULL,
    fallback SMALLINT NOT NULL DEFAULT 1 CHECK (fallback IN (1, 2)),
    CONSTRAINT credstore_secrets_value_version_check CHECK ((value_version IS NULL) = (status = 4))
);
                "
            ),
            // Only `active` rows are carried, and only as `declared`: the
            // store key shape changes from `tenant/reference/class` to
            // `(tenant_id, record_id)`, so no pre-migration value was ever
            // written under it - there is nothing a carried row could
            // correctly point at. `provisioning`/`deprovisioning` rows are
            // dropped outright (a provisioning row never became visible; a
            // deprovisioning row was already invisible to resolution). A
            // greenfield deploy has no rows at all, so this SELECT is
            // typically a no-op.
            r"
INSERT INTO credstore_secrets_new
    (id, tenant_id, reference, sharing, owner_id, status, created_at, updated_at,
     version, secret_type_uuid, expires_at, value_version, fallback)
SELECT
    id, tenant_id, reference, sharing, owner_id, 4, created_at, updated_at,
    version, secret_type_uuid, expires_at, NULL, 1
FROM credstore_secrets
WHERE status = 2;
            "
            .to_owned(),
            "DROP TABLE credstore_secrets;".to_owned(),
            "ALTER TABLE credstore_secrets_new RENAME TO credstore_secrets;".to_owned(),
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_nonprivate \
                ON credstore_secrets (tenant_id, reference) WHERE sharing <> 1;"
                .to_owned(),
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_private \
                ON credstore_secrets (tenant_id, reference, owner_id) WHERE sharing = 1;"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_lookup \
                ON credstore_secrets (reference, tenant_id, status);"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_expiry \
                ON credstore_secrets (expires_at) WHERE expires_at IS NOT NULL AND status = 2;"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_type \
                ON credstore_secrets (tenant_id, secret_type_uuid);"
                .to_owned(),
        ];
        for sql in &statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down_sqlite(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
        let generic_uuid_hex = GENERIC_TYPE_UUID_STR.replace('-', "");
        let statements = [
            format!(
                r"
CREATE TABLE credstore_secrets_old (
    id BLOB PRIMARY KEY NOT NULL,
    tenant_id BLOB NOT NULL,
    reference TEXT NOT NULL CHECK (length(reference) BETWEEN 1 AND 255),
    sharing SMALLINT NOT NULL CHECK (sharing IN (1, 2, 3)),
    owner_id BLOB NOT NULL,
    status SMALLINT NOT NULL CHECK (status IN (1, 2, 3)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 1,
    secret_type_uuid BLOB NOT NULL DEFAULT (x'{generic_uuid_hex}'),
    expires_at TEXT NULL,
    value_fp BLOB NULL,
    fp_key_id SMALLINT NULL,
    CHECK ((value_fp IS NULL) = (fp_key_id IS NULL))
);
                "
            ),
            // Only rows that already fit the old (status IN (1,2,3)) shape
            // carry back; a `declared` (4) row has no pre-ADR-0006
            // equivalent and is dropped.
            r"
INSERT INTO credstore_secrets_old
    (id, tenant_id, reference, sharing, owner_id, status, created_at, updated_at,
     version, secret_type_uuid, expires_at, value_fp, fp_key_id)
SELECT
    id, tenant_id, reference, sharing, owner_id, status, created_at, updated_at,
    version, secret_type_uuid, expires_at, value_fp, fp_key_id
FROM credstore_secrets
WHERE status IN (1, 2, 3);
            "
            .to_owned(),
            "DROP TABLE credstore_secrets;".to_owned(),
            "ALTER TABLE credstore_secrets_old RENAME TO credstore_secrets;".to_owned(),
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_nonprivate \
                ON credstore_secrets (tenant_id, reference) WHERE sharing <> 1;"
                .to_owned(),
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_private \
                ON credstore_secrets (tenant_id, reference, owner_id) WHERE sharing = 1;"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_lookup \
                ON credstore_secrets (reference, tenant_id, status);"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_pending \
                ON credstore_secrets (updated_at) WHERE status <> 2;"
                .to_owned(),
            "CREATE INDEX IF NOT EXISTS idx_credstore_expiry \
                ON credstore_secrets (expires_at) WHERE expires_at IS NOT NULL AND status = 2;"
                .to_owned(),
        ];
        for sql in &statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }
}

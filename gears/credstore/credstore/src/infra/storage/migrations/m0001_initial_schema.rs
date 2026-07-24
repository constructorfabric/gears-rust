//! Initial credstore schema — the `credstore_secrets` table with the full
//! lifecycle status set (`provisioning`/`active`/`deprovisioning`), the
//! monotonic `version` column, GTS secret typing (`secret_type_uuid`,
//! `expires_at`), the value-fingerprint fence columns (`value_fp`,
//! `fp_key_id` — see `docs/features/001-value-fingerprint-fence.md`), and
//! all indexes. Both fence columns are NULL together (out-of-band seeded
//! rows) or set together (API-written rows), enforced by CHECK.
//!
//! The secret type is stored as the deterministic v5 UUID of its GTS type
//! id (like `tenants.tenant_type_uuid` in AM) — compact, and resolvable to
//! the type id + traits via the types-registry at operation time.
//!
//! Per-backend raw `SQL` is used (not `SeaORM`'s schema-builder) so that
//! `CHECK` constraints and partial unique indexes are preserved verbatim.
//! `MySQL` is not supported; the migration fails fast with a typed error.

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

        // Default type for rows written before typed clients: the v5 UUID of
        // the `generic` type (exactly the untyped semantics). SQLite stores
        // UUIDs as 16-byte blobs, so its DEFAULT is the hex-blob literal of
        // the same pinned value.
        let generic_uuid_hex = GENERIC_TYPE_UUID_STR.replace('-', "");

        let table_sql = match backend {
            sea_orm::DatabaseBackend::Postgres => format!(
                r"
CREATE TABLE IF NOT EXISTS credstore_secrets (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    reference TEXT NOT NULL CHECK (length(reference) BETWEEN 1 AND 255),
    sharing SMALLINT NOT NULL CHECK (sharing IN (1, 2, 3)),
    owner_id UUID NOT NULL,
    status SMALLINT NOT NULL CHECK (status IN (1, 2, 3)),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version BIGINT NOT NULL DEFAULT 1,
    secret_type_uuid UUID NOT NULL DEFAULT '{GENERIC_TYPE_UUID_STR}',
    expires_at TIMESTAMPTZ NULL,
    value_fp BYTEA NULL,
    fp_key_id SMALLINT NULL,
    CHECK ((value_fp IS NULL) = (fp_key_id IS NULL))
);
                "
            ),
            sea_orm::DatabaseBackend::Sqlite => format!(
                r"
CREATE TABLE IF NOT EXISTS credstore_secrets (
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
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        let statements = [
            table_sql.as_str(),
            // Coexistence of a private and a tenant/shared secret under one reference:
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_nonprivate ON credstore_secrets (tenant_id, reference) WHERE sharing <> 1;",
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_credstore_private ON credstore_secrets (tenant_id, reference, owner_id) WHERE sharing = 1;",
            // Walk-up resolution:
            "CREATE INDEX IF NOT EXISTS idx_credstore_lookup ON credstore_secrets (reference, tenant_id, status);",
            // Reaper sweep over all non-active (saga) rows:
            "CREATE INDEX IF NOT EXISTS idx_credstore_pending ON credstore_secrets (updated_at) WHERE status <> 2;",
            // Reaper expiry sweep over active rows carrying an expiry:
            "CREATE INDEX IF NOT EXISTS idx_credstore_expiry ON credstore_secrets (expires_at) WHERE expires_at IS NOT NULL AND status = 2;",
        ];

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if matches!(
            manager.get_database_backend(),
            sea_orm::DatabaseBackend::MySql
        ) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS credstore_secrets;")
            .await?;
        Ok(())
    }
}

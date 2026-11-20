//! Convert RG's local catalogue to a GTS identity dictionary (T38b).
//!
//! Keep all `gts_type` IDs, identifiers and creation timestamps, including
//! unreferenced entries, and preserve groups, memberships, closure rows and
//! their foreign keys. Drop the placement-rule tables and the
//! `metadata_schema` and `updated_at` columns.
//!
//! The membership key includes a UUID, a type key and `resource_id`. The
//! 2600-byte UTF-8 limit leaves room for those fields and headers within the
//! PostgreSQL B-tree limit (2704 bytes on standard 8 KiB pages). Preflight
//! rejects oversized legacy IDs before DDL. PostgreSQL adds the check in
//! place. SQLite rebuilds the unreferenced membership table.
//!
//! Both backends apply the DDL and history record in one transaction. MySQL
//! is unsupported. Export custom definitions or back up the old database
//! before upgrading. Until owners publish them, existing rows remain readable
//! and removable, while new links, moves and metadata updates need a published
//! definition. Restore the backup to roll back because `down()` cannot
//! reconstruct the dropped rules.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

// Frozen with this migration; a future SDK limit change needs a new migration.
const MAX_MEMBERSHIP_RESOURCE_ID_BYTES: usize = 2600;

const POSTGRES_UP: &str = r"
DROP TABLE gts_type_allowed_membership;
DROP TABLE gts_type_allowed_parent;
ALTER TABLE gts_type DROP COLUMN metadata_schema, DROP COLUMN updated_at;
ALTER TABLE resource_group_membership
    ADD CONSTRAINT ck_rgm_resource_id_bytes CHECK (octet_length(resource_id) <= 2600);
";

const SQLITE_UP: &str = r"
DROP TABLE gts_type_allowed_membership;
DROP TABLE gts_type_allowed_parent;
ALTER TABLE gts_type DROP COLUMN metadata_schema;
ALTER TABLE gts_type DROP COLUMN updated_at;

CREATE TABLE resource_group_membership_new (
    group_id TEXT NOT NULL,
    gts_type_id INTEGER NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    CONSTRAINT ck_rgm_resource_id_bytes
        CHECK (length(CAST(resource_id AS BLOB)) <= 2600),
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_rgm_gts_type
        FOREIGN KEY (gts_type_id) REFERENCES gts_type(id) ON DELETE RESTRICT,
    PRIMARY KEY (group_id, gts_type_id, resource_id)
);
INSERT INTO resource_group_membership_new (group_id, gts_type_id, resource_id, created_at)
SELECT group_id, gts_type_id, resource_id, created_at FROM resource_group_membership;
DROP TABLE resource_group_membership;
ALTER TABLE resource_group_membership_new RENAME TO resource_group_membership;
CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let (sql, resource_bytes) = match backend {
            sea_orm::DatabaseBackend::Postgres => (POSTGRES_UP, "octet_length(resource_id)"),
            sea_orm::DatabaseBackend::Sqlite => (SQLITE_UP, "length(CAST(resource_id AS BLOB))"),
            _ => {
                return Err(DbErr::Migration(format!(
                    "{backend:?} is not a supported backend for resource-group"
                )));
            }
        };
        let conn = manager.get_connection();
        let preflight = format!(
            "SELECT 1 FROM resource_group_membership \
             WHERE {resource_bytes} > {MAX_MEMBERSHIP_RESOURCE_ID_BYTES} LIMIT 1"
        );
        if conn
            .query_one_raw(Statement::from_string(backend, preflight))
            .await?
            .is_some()
        {
            return Err(DbErr::Migration(format!(
                "{}: membership resource_id exceeds \
                 {MAX_MEMBERSHIP_RESOURCE_ID_BYTES} UTF-8 bytes; \
                 back up the database and remap oversized resource identifiers \
                 with their owning gear or remove the affected memberships \
                 before upgrading; the old schema and rows are unchanged",
                self.name()
            )));
        }
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} the dropped RG type definitions (metadata \
             schemas, placement rules) cannot be rebuilt from the identity \
             dictionary; restore the pre-upgrade backup instead",
            self.name()
        )))
    }
}

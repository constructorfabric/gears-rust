//! RG's type catalogue shrinks to a local identity dictionary (T38b: the types
//! registry is the only catalogue of type definitions).
//!
//! `gts_type` keeps `id` (the `SMALLINT` key groups and memberships reference
//! through their existing foreign keys), `schema_id` (the full GTS type
//! identifier, unique) and `created_at`. Everything that defined a type --
//! `metadata_schema`, the `gts_type_allowed_parent` / `gts_type_allowed_membership`
//! placement rules -- is dropped; `updated_at` goes too, as dictionary rows are
//! immutable. Groups, memberships, closure rows and every dictionary row stay
//! as they are, including rows for types nothing references any more: a
//! dictionary row only maps a key to an identifier and proves nothing about the
//! type's existence, admission or lifecycle in the registry.
//!
//! The membership primary key is `(group_id, gts_type_id, resource_id)`, so the
//! external resource identifier must fit PostgreSQL's B-tree item limit (2704
//! bytes on standard 8 KiB pages) together with the 16-byte UUID, the 2-byte
//! type key and the tuple and varlena headers. Both backends therefore bound
//! `resource_id` to 2600 UTF-8 bytes (`ck_rgm_resource_id_bytes`). A preflight
//! refuses old oversized identifiers before any DDL; remap or remove them with
//! the owning gear before upgrading. PostgreSQL adds the constraint in place;
//! SQLite cannot add a constraint, so it rebuilds the membership table (nothing
//! references it).
//!
//! The runtime applies the migration and its history record in one transaction
//! (DDL is transactional on both backends), so a refusal leaves the previous
//! schema untouched. MySQL is refused, as by the initial migration.
//!
//! The dropped definitions are not carried anywhere: the migration cannot
//! publish them to the types registry. Types defined only in an old RG database
//! must be exported (or the database backed up) before upgrading and published
//! by their owners. Until then their groups and memberships stay readable and
//! deletable, and refuse new links, moves and metadata updates.
//!
//! `down()` refuses: the definitions cannot be rebuilt from the identifiers
//! that remain, and recreating empty rule tables would hand the previous
//! release a catalogue that silently says "no parents, no members" for every
//! type. Roll back by restoring the pre-upgrade backup.

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

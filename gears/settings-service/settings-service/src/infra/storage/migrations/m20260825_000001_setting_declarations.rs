// Created: 2026-08-25 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-entity-schema:p1
//! The `setting_declarations` table.
//!
//! Shape from DESIGN.md §4.7. Two things here are load-bearing beyond storage.
//!
//! The `categories` foreign key is `ON DELETE RESTRICT`, which is what makes the
//! no-orphan rule the database's to enforce rather than the handler's to
//! remember. The service checks first only so the common case reports a conflict
//! instead of a constraint error; a declaration inserted between that check and
//! the delete is caught here.
//!
//! The cross-field checks live in the schema for the same reason. A `secret`
//! classification that disagreed with `has_secret_trait`, or an anonymous-exposable
//! flag on a `secret` or `pii` declaration, would be a contradiction no
//! application path is allowed to write — and stating it once in the column
//! definition is stronger than restating it at every call site.
//!
//! `SQLite` backs the test suite and does not enforce foreign keys unless
//! `PRAGMA foreign_keys = ON` is set on the connection. The constraint is
//! declared on both backends so the schemas do not diverge, but only Postgres
//! is relied on to enforce it; the guard in the category service is what covers
//! the test backend.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = if backend == DatabaseBackend::Postgres {
            vec![
                r"CREATE TABLE IF NOT EXISTS setting_declarations (
                    id                   uuid          PRIMARY KEY DEFAULT gen_random_uuid(),
                    key                  text          NOT NULL,
                    leaf_slug            text          NOT NULL,
                    value_type_id        text          NOT NULL,
                    category_id          uuid          NOT NULL
                                         REFERENCES categories (id) ON DELETE RESTRICT,
                    default_value        jsonb         NOT NULL,
                    scope_class          text          NOT NULL
                                         CHECK (scope_class IN ('global', 'cascading', 'local')),
                    mode                 text          NOT NULL DEFAULT 'standard'
                                         CHECK (mode IN ('standard', 'advanced')),
                    domain_affinity      text,
                    has_secret_trait     boolean       NOT NULL DEFAULT false,
                    requires_step_up     boolean       NOT NULL DEFAULT true,
                    anonymous_exposable  boolean       NOT NULL DEFAULT false,
                    data_classification  text          NOT NULL DEFAULT 'public'
                                         CHECK (data_classification IN ('public', 'pii', 'secret')),
                    source               text          NOT NULL DEFAULT 'admin_authored'
                                         CHECK (source IN ('admin_authored', 'module_contributed')),
                    owner_module         text,
                    licence_feature      text,
                    status               text          NOT NULL DEFAULT 'active'
                                         CHECK (status IN ('active', 'retired')),
                    description          varchar(4096),
                    last_change_at       timestamptz   NOT NULL DEFAULT now(),
                    created_at           timestamptz   NOT NULL DEFAULT now(),
                    updated_at           timestamptz   NOT NULL DEFAULT now(),
                    created_by           text          NOT NULL,
                    CONSTRAINT ck_declaration_exposable_not_sensitive
                        CHECK (NOT (anonymous_exposable AND data_classification IN ('secret', 'pii'))),
                    CONSTRAINT ck_declaration_secret_matches_trait
                        CHECK ((data_classification = 'secret') = has_secret_trait),
                    CONSTRAINT ck_declaration_owner_module_iff_contributed
                        CHECK ((source = 'module_contributed') = (owner_module IS NOT NULL))
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_declaration_key
                     ON setting_declarations (key);",
                // Per category, not globally: two categories may each hold a
                // setting named `timeout`, and their keys differ by the category
                // segment they embed. Partial on the active rows: a retired
                // predecessor must not hold its name against its own successor
                // (DESIGN.md §4.3 *Re-declare to revive*, §4.7).
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_declaration_category_slug
                     ON setting_declarations (category_id, leaf_slug)
                     WHERE status = 'active';",
                "CREATE INDEX IF NOT EXISTS idx_declarations_category
                     ON setting_declarations (category_id);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_owner_module
                     ON setting_declarations (owner_module);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_domain
                     ON setting_declarations (domain_affinity);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_mode
                     ON setting_declarations (mode);",
                // Retirement is a soft delete, so nearly every read filters on
                // `active`. A partial index keeps retired rows out of it.
                "CREATE INDEX IF NOT EXISTS idx_declarations_active
                     ON setting_declarations (status) WHERE status = 'active';",
                "CREATE INDEX IF NOT EXISTS idx_declarations_key_trgm
                     ON setting_declarations USING gin (key gin_trgm_ops);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_desc_trgm
                     ON setting_declarations USING gin (description gin_trgm_ops);",
                // Schema Defaults are searchable on the same terms as overrides.
                // The `jsonb_typeof` term keeps a JSON-`null` default out of the
                // corpus: the column is NOT NULL, so a setting with no meaningful
                // default holds `'null'::jsonb`, whose text projection is the
                // literal `null` -- indexing it would make the query `null` match
                // every such setting.
                "CREATE INDEX IF NOT EXISTS idx_declarations_default_trgm
                     ON setting_declarations USING gin ((default_value #>> '{}') gin_trgm_ops)
                     WHERE data_classification = 'public'
                       AND jsonb_typeof(default_value) <> 'null';",
                "CREATE INDEX IF NOT EXISTS idx_declarations_default_pii_trgm
                     ON setting_declarations USING gin ((default_value #>> '{}') gin_trgm_ops)
                     WHERE data_classification = 'pii'
                       AND jsonb_typeof(default_value) <> 'null';",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS setting_declarations (
                    id                   text     PRIMARY KEY,
                    key                  text     NOT NULL,
                    leaf_slug            text     NOT NULL,
                    value_type_id        text     NOT NULL,
                    category_id          text     NOT NULL
                                         REFERENCES categories (id) ON DELETE RESTRICT,
                    default_value        text     NOT NULL,
                    scope_class          text     NOT NULL
                                         CHECK (scope_class IN ('global', 'cascading', 'local')),
                    mode                 text     NOT NULL DEFAULT 'standard'
                                         CHECK (mode IN ('standard', 'advanced')),
                    domain_affinity      text,
                    has_secret_trait     integer  NOT NULL DEFAULT 0,
                    requires_step_up     integer  NOT NULL DEFAULT 1,
                    anonymous_exposable  integer  NOT NULL DEFAULT 0,
                    data_classification  text     NOT NULL DEFAULT 'public'
                                         CHECK (data_classification IN ('public', 'pii', 'secret')),
                    source               text     NOT NULL DEFAULT 'admin_authored'
                                         CHECK (source IN ('admin_authored', 'module_contributed')),
                    owner_module         text,
                    licence_feature      text,
                    status               text     NOT NULL DEFAULT 'active'
                                         CHECK (status IN ('active', 'retired')),
                    description          text,
                    last_change_at       text     NOT NULL,
                    created_at           text     NOT NULL,
                    updated_at           text     NOT NULL,
                    created_by           text     NOT NULL,
                    CONSTRAINT ck_declaration_exposable_not_sensitive
                        CHECK (NOT (anonymous_exposable = 1 AND data_classification IN ('secret', 'pii'))),
                    CONSTRAINT ck_declaration_secret_matches_trait
                        CHECK ((data_classification = 'secret') = has_secret_trait),
                    CONSTRAINT ck_declaration_owner_module_iff_contributed
                        CHECK ((source = 'module_contributed') = (owner_module IS NOT NULL))
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_declaration_key
                     ON setting_declarations (key);",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_declaration_category_slug
                     ON setting_declarations (category_id, leaf_slug)
                     WHERE status = 'active';",
                "CREATE INDEX IF NOT EXISTS idx_declarations_category
                     ON setting_declarations (category_id);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_owner_module
                     ON setting_declarations (owner_module);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_domain
                     ON setting_declarations (domain_affinity);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_mode
                     ON setting_declarations (mode);",
                "CREATE INDEX IF NOT EXISTS idx_declarations_active
                     ON setting_declarations (status) WHERE status = 'active';",
            ]
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS setting_declarations;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260825_000001_setting_declarations_tests.rs"]
mod setting_declarations_tests;

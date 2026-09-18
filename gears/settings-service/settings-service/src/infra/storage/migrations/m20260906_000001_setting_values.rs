// Created: 2026-09-06 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-value-schema:p1
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-scope-invariants:p1
//! The `setting_values` table.
//!
//! Shape from DESIGN.md §4.7. Three things here are load-bearing beyond
//! storage.
//!
//! **Scope is an id, never `NULL` and never a path.** The root tenant carries
//! platform scope, so a platform-level value is an ordinary member of every
//! ancestor chain and the cascade query is one `IN` over ancestor ids. A `NULL`
//! there would put the row outside every filter `AccessScope` can express, and a
//! path would break under a re-parent; `tenant_id` is not a foreign key because
//! tenants live in another gear's schema.
//!
//! **Exactly one of `value` and `secret_ref` is set**, and which one follows
//! the declaration's classification, denormalized onto this row. SQL `NULL` in
//! `value` means *no inline value here* — a setting whose type admits `null`
//! stores the JSON value `null`, a non-`NULL` column, and the check reads it as
//! a value like any other.
//!
//! **Uniqueness is two partial indexes, one per scope shape.** Only the subject
//! halves may be `NULL`, and a plain unique index treats `NULL`s as distinct, so
//! the shape without a subject needs its own predicate. The subject columns
//! exist from v1 with nothing writing them: retrofitting a column into a unique
//! index on a live table is the data migration the design forbids.
//!
//! `SQLite` backs the test suite; the `PostgreSQL` trigram indexes that split the
//! search corpus by classification have no `SQLite` counterpart and are omitted
//! there.

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
                r"CREATE TABLE IF NOT EXISTS setting_values (
                    id                   uuid          PRIMARY KEY DEFAULT gen_random_uuid(),
                    declaration_id       uuid          NOT NULL
                                         REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id            uuid          NOT NULL,
                    subject_type         text,
                    subject_id           text,
                    value                jsonb,
                    secret_ref           text,
                    data_classification  text          NOT NULL DEFAULT 'public'
                                         CHECK (data_classification IN ('public', 'pii', 'secret')),
                    needs_review         boolean       NOT NULL DEFAULT false,
                    needs_review_detail  text,
                    last_change_at       timestamptz   NOT NULL DEFAULT now(),
                    created_at           timestamptz   NOT NULL DEFAULT now(),
                    updated_at           timestamptz   NOT NULL DEFAULT now(),
                    set_by               text          NOT NULL,
                    CONSTRAINT ck_value_exactly_one
                        CHECK (num_nonnulls(value, secret_ref) = 1),
                    CONSTRAINT ck_value_secret_matches_classification
                        CHECK ((data_classification = 'secret') = (secret_ref IS NOT NULL)),
                    CONSTRAINT ck_value_subject_both_or_neither
                        CHECK ((subject_type IS NULL) = (subject_id IS NULL))
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_value_scope
                     ON setting_values (declaration_id, tenant_id)
                     WHERE subject_type IS NULL;",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_value_scope_subject
                     ON setting_values (declaration_id, tenant_id, subject_type, subject_id)
                     WHERE subject_type IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_values_declaration
                     ON setting_values (declaration_id);",
                "CREATE INDEX IF NOT EXISTS idx_values_needs_review
                     ON setting_values (declaration_id, tenant_id)
                     WHERE needs_review;",
                "CREATE INDEX IF NOT EXISTS idx_values_value_trgm
                     ON setting_values USING gin ((value #>> '{}') gin_trgm_ops)
                     WHERE secret_ref IS NULL AND data_classification = 'public';",
                "CREATE INDEX IF NOT EXISTS idx_values_value_pii_trgm
                     ON setting_values USING gin ((value #>> '{}') gin_trgm_ops)
                     WHERE secret_ref IS NULL AND data_classification = 'pii';",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS setting_values (
                    id                   text     PRIMARY KEY,
                    declaration_id       text     NOT NULL
                                         REFERENCES setting_declarations (id) ON DELETE CASCADE,
                    tenant_id            text     NOT NULL,
                    subject_type         text,
                    subject_id           text,
                    value                text,
                    secret_ref           text,
                    data_classification  text     NOT NULL DEFAULT 'public'
                                         CHECK (data_classification IN ('public', 'pii', 'secret')),
                    needs_review         integer  NOT NULL DEFAULT 0,
                    needs_review_detail  text,
                    last_change_at       text     NOT NULL,
                    created_at           text     NOT NULL,
                    updated_at           text     NOT NULL,
                    set_by               text     NOT NULL,
                    CONSTRAINT ck_value_exactly_one
                        CHECK ((value IS NOT NULL) + (secret_ref IS NOT NULL) = 1),
                    CONSTRAINT ck_value_secret_matches_classification
                        CHECK ((data_classification = 'secret') = (secret_ref IS NOT NULL)),
                    CONSTRAINT ck_value_subject_both_or_neither
                        CHECK ((subject_type IS NULL) = (subject_id IS NULL))
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_value_scope
                     ON setting_values (declaration_id, tenant_id)
                     WHERE subject_type IS NULL;",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_value_scope_subject
                     ON setting_values (declaration_id, tenant_id, subject_type, subject_id)
                     WHERE subject_type IS NOT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_values_declaration
                     ON setting_values (declaration_id);",
                "CREATE INDEX IF NOT EXISTS idx_values_needs_review
                     ON setting_values (declaration_id, tenant_id)
                     WHERE needs_review = 1;",
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
            .execute_unprepared("DROP TABLE IF EXISTS setting_values;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260906_000001_setting_values_tests.rs"]
mod m20260906_000001_setting_values_tests;

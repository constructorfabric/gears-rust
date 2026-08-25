// Created: 2026-08-13 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-category-management-entity-schema:p1
//! The `categories` table.
//!
//! Shape from DESIGN.md §4.7. Categories are **flat** — no parent column, no
//! closure table — which is why `name` can be globally unique rather than
//! unique within a parent.

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

        // SQLite backs the test suite: no extensions, application-supplied
        // UUIDs, and no trigram index. The columns and constraints that carry
        // behaviour are identical on both.
        let statements: Vec<&str> = if backend == DatabaseBackend::Postgres {
            vec![
                r"CREATE TABLE IF NOT EXISTS categories (
                    id               uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
                    key              varchar(128) NOT NULL,
                    name             varchar(256) NOT NULL,
                    description      varchar(4096),
                    domain_affinity  text,
                    sort_order       integer      NOT NULL DEFAULT 0,
                    icon             text,
                    created_at       timestamptz  NOT NULL DEFAULT now(),
                    updated_at       timestamptz  NOT NULL DEFAULT now()
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_category_key ON categories (key);",
                // Globally unique because categories are flat: with no parent to
                // scope it, two categories sharing a name are indistinguishable
                // to the administrator choosing between them.
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_category_name ON categories (name);",
                // Trigram search, which a later wave builds on. The extension it
                // needs is created by the foundation's own migration, since no
                // single feature can own something every feature may want.
                "CREATE INDEX IF NOT EXISTS idx_categories_name_trgm
                     ON categories USING gin (name gin_trgm_ops);",
            ]
        } else {
            vec![
                r"CREATE TABLE IF NOT EXISTS categories (
                    id               text     PRIMARY KEY,
                    key              text     NOT NULL,
                    name             text     NOT NULL,
                    description      text,
                    domain_affinity  text,
                    sort_order       integer  NOT NULL DEFAULT 0,
                    icon             text,
                    created_at       text     NOT NULL,
                    updated_at       text     NOT NULL
                );",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_category_key ON categories (key);",
                "CREATE UNIQUE INDEX IF NOT EXISTS uq_category_name ON categories (name);",
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
            .execute_unprepared("DROP TABLE IF EXISTS categories;")
            .await?;
        Ok(())
    }
}

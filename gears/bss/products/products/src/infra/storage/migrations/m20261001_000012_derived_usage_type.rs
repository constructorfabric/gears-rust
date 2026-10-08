//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-store:p1
//! Create the derived usage type store (P-D-229, P-D-231): `bss.products_derived_usage_type`, one
//! row per type, and `bss.products_derived_usage_type_version`, its append-only versions.
//!
//! # The type
//!
//! `(tenant_id, id)` is the key, and `(tenant_id, code)` is unique (`uq_products_derived_usage_type_code`):
//! the meter id `products.derived/<code>@<n>` names a type by its code, per tenant. The code CHECK
//! holds `^[a-z0-9][a-z0-9._-]{0,63}$` physically; the door judges it first. `name` is set at the
//! create and no door changes it (O-1: a type has no lifecycle).
//!
//! # The versions
//!
//! `(tenant_id, type_id, version)` is the key, and `(tenant_id, type_id)` references the type: the
//! foreign key is tenant-qualified, so a version can never name another tenant's type.
//! `declaration_json` is the declaration as the doors serve it; `digest` is the SHA-256 of the SDK's
//! canonical bytes, as 64 lowercase hex digits, stored once at insert. The digest is never
//! recomputed on a read (P-D-229 decision 3).
//!
//! **A version is append-only.** Postgres refuses every `UPDATE` and `DELETE` through one
//! `PL/pgSQL` function, as pricing's usage rating policy table does (`m20260930_000018`); `SQLite`
//! mirrors it with two triggers and fixed messages. The type row has no trigger: no door writes it
//! after the create, and its versions' foreign key holds it in place.
//!
//! # Backend differences
//!
//! `uuid` becomes `text`, `jsonb` becomes `text`, `timestamptz` becomes `text`, the `bss.`
//! qualification is dropped, and the two regular-expression CHECKs become `GLOB` and `length`
//! tests. Every key, index and CHECK is on both sides.
//!
//! # Down
//!
//! Reversible: the versions, then the type, and on Postgres the trigger's function.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bss.products_derived_usage_type (
            tenant_id   uuid        NOT NULL,
            id          uuid        NOT NULL,
            code        text        NOT NULL,
            name        text        NOT NULL,
            created_by  uuid        NOT NULL,
            created_at  timestamptz NOT NULL,
            CONSTRAINT products_derived_usage_type_pkey PRIMARY KEY (tenant_id, id),
            CONSTRAINT chk_products_derived_usage_type_code CHECK (code ~ '^[a-z0-9][a-z0-9._-]{0,63}$')
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_derived_usage_type_code ON bss.products_derived_usage_type USING btree (tenant_id, code)",
    "CREATE TABLE IF NOT EXISTS bss.products_derived_usage_type_version (
            tenant_id         uuid        NOT NULL,
            type_id           uuid        NOT NULL,
            version           bigint      NOT NULL,
            declaration_json  jsonb       NOT NULL,
            digest            text        NOT NULL,
            created_by        uuid        NOT NULL,
            created_at        timestamptz NOT NULL,
            CONSTRAINT products_derived_usage_type_version_pkey PRIMARY KEY (tenant_id, type_id, version),
            CONSTRAINT fk_products_derived_usage_type_version_type FOREIGN KEY (tenant_id, type_id)
                REFERENCES bss.products_derived_usage_type (tenant_id, id),
            CONSTRAINT chk_products_derived_usage_type_version_version CHECK (version >= 1),
            CONSTRAINT chk_products_derived_usage_type_version_digest CHECK (digest ~ '^[0-9a-f]{64}$')
        )",
    "CREATE OR REPLACE FUNCTION bss.products_derived_usage_type_version_append_only() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'products_derived_usage_type_version is append-only: % is not permitted', TG_OP; END; $$",
    "DROP TRIGGER IF EXISTS products_derived_usage_type_version_append_only ON bss.products_derived_usage_type_version",
    "CREATE TRIGGER products_derived_usage_type_version_append_only BEFORE UPDATE OR DELETE ON bss.products_derived_usage_type_version FOR EACH ROW EXECUTE FUNCTION bss.products_derived_usage_type_version_append_only()",
];

const PG_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS bss.products_derived_usage_type_version",
    "DROP FUNCTION IF EXISTS bss.products_derived_usage_type_version_append_only()",
    "DROP TABLE IF EXISTS bss.products_derived_usage_type",
];

const SQLITE_UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS products_derived_usage_type (
            tenant_id   text NOT NULL,
            id          text NOT NULL,
            code        text NOT NULL,
            name        text NOT NULL,
            created_by  text NOT NULL,
            created_at  text NOT NULL,
            PRIMARY KEY (tenant_id, id),
            CONSTRAINT chk_products_derived_usage_type_code CHECK (
                length(code) BETWEEN 1 AND 64
                AND code GLOB '[a-z0-9]*'
                AND code NOT GLOB '*[^a-z0-9._-]*'
            )
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS uq_products_derived_usage_type_code ON products_derived_usage_type (tenant_id, code)",
    "CREATE TABLE IF NOT EXISTS products_derived_usage_type_version (
            tenant_id         text    NOT NULL,
            type_id           text    NOT NULL,
            version           integer NOT NULL,
            declaration_json  text    NOT NULL,
            digest            text    NOT NULL,
            created_by        text    NOT NULL,
            created_at        text    NOT NULL,
            PRIMARY KEY (tenant_id, type_id, version),
            CONSTRAINT fk_products_derived_usage_type_version_type FOREIGN KEY (tenant_id, type_id)
                REFERENCES products_derived_usage_type (tenant_id, id),
            CONSTRAINT chk_products_derived_usage_type_version_version CHECK (version >= 1),
            CONSTRAINT chk_products_derived_usage_type_version_digest CHECK (
                length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'
            )
        )",
    "DROP TRIGGER IF EXISTS products_derived_usage_type_version_no_update",
    "CREATE TRIGGER products_derived_usage_type_version_no_update BEFORE UPDATE ON products_derived_usage_type_version BEGIN SELECT RAISE(ABORT, 'products_derived_usage_type_version is append-only: UPDATE is not permitted'); END",
    "DROP TRIGGER IF EXISTS products_derived_usage_type_version_no_delete",
    "CREATE TRIGGER products_derived_usage_type_version_no_delete BEFORE DELETE ON products_derived_usage_type_version BEGIN SELECT RAISE(ABORT, 'products_derived_usage_type_version is append-only: DELETE is not permitted'); END",
];

const SQLITE_DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS products_derived_usage_type_version",
    "DROP TABLE IF EXISTS products_derived_usage_type",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

#[cfg(test)]
#[path = "m20261001_000012_derived_usage_type_tests.rs"]
mod tests;

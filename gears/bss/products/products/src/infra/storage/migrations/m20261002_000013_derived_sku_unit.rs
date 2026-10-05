//! A derived usage SKU stores no unit (P-D-259). The unit a read serves is the referenced
//! version's `output_unit`.
//!
//! Before the update, every derived SKU whose stored unit is not null is checked against that
//! version. A row whose unit disagrees, or whose version is missing, refuses the migration with
//! the SKU id and is not nulled. Agreeing rows are then set to null, and a CHECK (Postgres) or
//! two triggers (SQLite, which cannot add a CHECK to an existing table) keeps a derived
//! `usage_type_ref` from being stored with a unit.
//!
//! # Down
//!
//! The CHECK or the triggers go away. The nulled units are not restored: the version still holds
//! the unit a read serves.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

use super::exec_backend;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "UPDATE bss.products_sku SET unit = NULL \
     WHERE usage_type_ref LIKE 'products.derived/%' AND unit IS NOT NULL",
    "ALTER TABLE bss.products_sku ADD CONSTRAINT chk_products_sku_derived_unit CHECK ( \
        usage_type_ref IS NULL \
        OR usage_type_ref NOT LIKE 'products.derived/%' \
        OR unit IS NULL)",
];

const PG_DOWN: &[&str] =
    &["ALTER TABLE bss.products_sku DROP CONSTRAINT IF EXISTS chk_products_sku_derived_unit"];

const SQLITE_UP: &[&str] = &[
    "UPDATE products_sku SET unit = NULL \
     WHERE usage_type_ref LIKE 'products.derived/%' AND unit IS NOT NULL",
    "DROP TRIGGER IF EXISTS products_sku_derived_unit_insert",
    "CREATE TRIGGER products_sku_derived_unit_insert BEFORE INSERT ON products_sku \
     WHEN NEW.usage_type_ref IS NOT NULL \
      AND NEW.usage_type_ref LIKE 'products.derived/%' \
      AND NEW.unit IS NOT NULL \
     BEGIN SELECT RAISE(ABORT, 'a derived usage SKU stores no unit'); END",
    "DROP TRIGGER IF EXISTS products_sku_derived_unit_update",
    "CREATE TRIGGER products_sku_derived_unit_update BEFORE UPDATE ON products_sku \
     WHEN NEW.usage_type_ref IS NOT NULL \
      AND NEW.usage_type_ref LIKE 'products.derived/%' \
      AND NEW.unit IS NOT NULL \
     BEGIN SELECT RAISE(ABORT, 'a derived usage SKU stores no unit'); END",
];

const SQLITE_DOWN: &[&str] = &[
    "DROP TRIGGER IF EXISTS products_sku_derived_unit_insert",
    "DROP TRIGGER IF EXISTS products_sku_derived_unit_update",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // PROBE-9-13-7: a disagreeing row refuses the migration and is not nulled.
        refuse_a_disagreeing_unit(manager).await?;
        exec_backend(self.name(), manager, PG_UP, SQLITE_UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        exec_backend(self.name(), manager, PG_DOWN, SQLITE_DOWN).await
    }
}

/// Every derived SKU that still stores a unit must store its version's `output_unit`.
async fn refuse_a_disagreeing_unit(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let conn = manager.get_connection();
    let (skus, versions) = match backend {
        DbBackend::Postgres => (
            "SELECT s.id::text AS id, s.tenant_id::text AS tenant_id, s.usage_type_ref AS reference, s.unit AS unit \
             FROM bss.products_sku s \
             WHERE s.usage_type_ref LIKE 'products.derived/%' AND s.unit IS NOT NULL",
            "SELECT t.tenant_id::text AS tenant_id, t.code AS code, v.version::text AS version, \
                    v.declaration_json->>'output_unit' AS output_unit \
             FROM bss.products_derived_usage_type t \
             JOIN bss.products_derived_usage_type_version v \
               ON v.tenant_id = t.tenant_id AND v.type_id = t.id",
        ),
        DbBackend::Sqlite => (
            "SELECT s.id AS id, s.tenant_id AS tenant_id, s.usage_type_ref AS reference, s.unit AS unit \
             FROM products_sku s \
             WHERE s.usage_type_ref LIKE 'products.derived/%' AND s.unit IS NOT NULL",
            "SELECT t.tenant_id AS tenant_id, t.code AS code, CAST(v.version AS TEXT) AS version, \
                    json_extract(v.declaration_json, '$.output_unit') AS output_unit \
             FROM products_derived_usage_type t \
             JOIN products_derived_usage_type_version v \
               ON v.tenant_id = t.tenant_id AND v.type_id = t.id",
        ),
        _ => {
            return Err(DbErr::Migration(format!(
                "{backend:?} is not a supported backend for bss-products"
            )));
        }
    };
    let mut output = std::collections::HashMap::<String, String>::new();
    for row in conn
        .query_all_raw(Statement::from_string(backend, versions.to_owned()))
        .await?
    {
        let tenant: String = row.try_get("", "tenant_id")?;
        let code: String = row.try_get("", "code")?;
        let version: String = row.try_get("", "version")?;
        let unit: Option<String> = row.try_get("", "output_unit")?;
        if let Some(unit) = unit {
            output.insert(format!("{tenant}|products.derived/{code}@{version}"), unit);
        }
    }
    for row in conn
        .query_all_raw(Statement::from_string(backend, skus.to_owned()))
        .await?
    {
        let id: String = row.try_get("", "id")?;
        let tenant: String = row.try_get("", "tenant_id")?;
        let reference: String = row.try_get("", "reference")?;
        let unit: String = row.try_get("", "unit")?;
        let key = format!("{tenant}|{reference}");
        if output.get(&key).map(String::as_str) != Some(unit.as_str()) {
            return Err(DbErr::Migration(format!(
                "m20261002_000013_derived_sku_unit: SKU {id} stored unit {unit} is not its version's output unit"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "m20261002_000013_derived_sku_unit_tests.rs"]
mod tests;

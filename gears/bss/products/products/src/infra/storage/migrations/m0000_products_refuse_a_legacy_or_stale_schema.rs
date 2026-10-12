//! The schema guard (P-D-195, pricing D-423): refuse a legacy or stale products schema before
//! anything of the gear is created.
//!
//! The toolkit runner sorts a gear's migrations by name and runs the pending ones, so this one —
//! `m0000_…` — runs before the coordination, broker and outbox migrations (`m0001_…`, `m001_…`)
//! and before `m20260925_000001`. It is pending on every database that predates phase 4 and runs
//! there once. It creates nothing and reads the catalog only: `sqlite_master` and `table_info` on
//! `SQLite`; `information_schema` and `pg_constraint` on Postgres, tables in schema `bss`.
//!
//! What it refuses:
//! - a table of [`LEGACY_TABLES`]: the legacy chain's tables that today's chain does not create
//!   (`products_sku`, `products_category`, `products_audit_log`, `products_idempotency` and
//!   `bss_approval::ddl`'s `products_approval_decision` exist in both and are not evidence);
//! - the legacy shape of a table name both chains create: `products_category` or `products_sku`
//!   without `code`. A clean-up that dropped only the tables a refusal named leaves them, and
//!   `m20260925_000001`/`000002`'s `CREATE TABLE IF NOT EXISTS` would keep them and fail on their
//!   `code` indexes (phase 4 review F2);
//! - `products_sku_reference` whose `ref_kind` CHECK does not admit `price_book_entry`: the phase 2
//!   rename edited `m20260925_000006` in place, and `CREATE TABLE IF NOT EXISTS` kept the old CHECK.
//!
//! A fresh database passes, and so does one migrated by today's chain.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The gear, as the refusal names it.
const GEAR: &str = "bss-products";

/// The legacy chain's tables minus today's (P-D-195).
///
/// The chain is `bss/products-backup:gears/bss/products/products/src/infra/storage/migrations/`
/// (`m20260829_000001` to `m20260922_000031`): 40 `products_*` tables, of which today's chain creates
/// five too. Sorted; the fast-tier suite re-derives this list from its own census and a fresh
/// chain and compares.
pub const LEGACY_TABLES: &[&str] = &[
    "products_approval",
    "products_attribute_definition",
    "products_attribute_value",
    "products_breakglass_session",
    "products_bulk_batch",
    "products_bulk_row",
    "products_catalog_version",
    "products_catalog_version_capture",
    "products_catalog_version_counter",
    "products_catalog_version_entry",
    "products_catalog_version_request",
    "products_correction_override",
    "products_deferred_retirement",
    "products_entity_version",
    "products_freeze_ack",
    "products_freeze_participant",
    "products_identity_ref",
    "products_materiality_policy",
    "products_metadata",
    "products_pii_allowlist",
    "products_product",
    "products_product_category",
    "products_read_checkpoint",
    "products_read_deferred_intent",
    "products_read_delivery_state",
    "products_read_entity",
    "products_read_freeze_status",
    "products_read_inbox",
    "products_read_poison",
    "products_read_stamp",
    "products_recognized_set",
    "products_reference_member",
    "products_reference_producer",
    "products_reference_watermark",
    "products_scheduled_transition",
];

/// The reference table whose kind CHECK the phase 2 rename edited in place.
const REFERENCE_TABLE: &str = "products_sku_reference";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let tables = tables(manager).await?;
        let legacy: Vec<&str> = tables
            .iter()
            .map(String::as_str)
            .filter(|t| LEGACY_TABLES.contains(t))
            .collect();
        if !legacy.is_empty() {
            return Err(refusal("legacy", &tables_found(&legacy)));
        }
        let mut stale = Vec::new();
        for (table, column) in [("products_category", "code"), ("products_sku", "code")] {
            if tables.iter().any(|t| t == table)
                && !columns(manager, table).await?.iter().any(|c| c == column)
            {
                stale.push(format!("{table} without column {column}"));
            }
        }
        if tables.iter().any(|t| t == REFERENCE_TABLE)
            && !admits_price_book_entry(&ref_kind_checks(manager).await?)
        {
            stale.push(
                "products_sku_reference whose ref_kind CHECK does not admit price_book_entry"
                    .to_owned(),
            );
        }
        if !stale.is_empty() {
            return Err(refusal("stale", &stale.join("; ")));
        }
        Ok(())
    }

    /// Nothing was created, so nothing is reversed.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// P-D-195's refusal.
fn refusal(kind: &str, found: &str) -> DbErr {
    DbErr::Migration(format!(
        "{GEAR}: this database holds a {kind} {GEAR} schema ({found}); PriceBook does not migrate \
         it \u{2014} start from an empty data root / empty {GEAR} tables"
    ))
}

/// `table a` or `tables a, b` — the names arrive sorted.
fn tables_found(names: &[&str]) -> String {
    match names {
        [one] => format!("table {one}"),
        _ => format!("tables {}", names.join(", ")),
    }
}

/// Every `ref_kind` CHECK admits `price_book_entry` (a table with none admits every kind).
fn admits_price_book_entry(checks: &[String]) -> bool {
    checks.iter().all(|c| c.contains("'price_book_entry'"))
}

/// The gear's tables, by name, sorted: `sqlite_master` on `SQLite`, schema `bss` on Postgres.
async fn tables(manager: &SchemaManager<'_>) -> Result<Vec<String>, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 9) = \
             'products_'"
        }
        DatabaseBackend::Postgres => {
            "SELECT table_name::text AS v FROM information_schema.tables WHERE table_schema = \
             'bss' AND left(table_name::text, 9) = 'products_'"
        }
        _ => return Err(unsupported(backend)),
    };
    // Sorted here, not by the engine: a Postgres collation may order `_` differently.
    let mut names = strings(manager, backend, sql.to_owned()).await?;
    names.sort_unstable();
    Ok(names)
}

/// The columns of one of the gear's tables.
async fn columns(manager: &SchemaManager<'_>, table: &str) -> Result<Vec<String>, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => format!("SELECT name AS v FROM pragma_table_info('{table}')"),
        DatabaseBackend::Postgres => format!(
            "SELECT column_name::text AS v FROM information_schema.columns WHERE table_schema = \
             'bss' AND table_name = '{table}'"
        ),
        _ => return Err(unsupported(backend)),
    };
    strings(manager, backend, sql).await
}

/// The text of every CHECK on the reference table that names `ref_kind`: parsed out of the
/// stored DDL on `SQLite`, one `pg_get_constraintdef` per constraint on Postgres.
async fn ref_kind_checks(manager: &SchemaManager<'_>) -> Result<Vec<String>, DbErr> {
    let backend = manager.get_database_backend();
    let checks = match backend {
        DatabaseBackend::Sqlite => {
            let ddl = strings(
                manager,
                backend,
                format!(
                    "SELECT sql AS v FROM sqlite_master WHERE type = 'table' AND name = \
                     '{REFERENCE_TABLE}'"
                ),
            )
            .await?;
            ddl.iter()
                .flat_map(|sql| check_bodies(sql))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        }
        DatabaseBackend::Postgres => {
            strings(
                manager,
                backend,
                format!(
                    "SELECT pg_get_constraintdef(con.oid) AS v FROM pg_constraint con \
                     JOIN pg_class c ON c.oid = con.conrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = 'bss' AND c.relname = '{REFERENCE_TABLE}' \
                     AND con.contype = 'c'"
                ),
            )
            .await?
        }
        _ => return Err(unsupported(backend)),
    };
    Ok(checks
        .into_iter()
        .filter(|c| c.contains("ref_kind"))
        .collect())
}

/// The parenthesised body of every `CHECK (…)` in a `CREATE TABLE` statement, quotes respected.
fn check_bodies(ddl: &str) -> Vec<&str> {
    let lower = ddl.to_ascii_lowercase();
    let bytes = ddl.as_bytes();
    let mut bodies = Vec::new();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find("check")) {
        let at = from + found;
        from = at + "check".len();
        let word_start = at == 0 || !is_ident(bytes[at - 1]);
        let Some(open) = lower
            .get(from..)
            .and_then(|rest| rest.find(|c: char| !c.is_whitespace()))
            .map(|skip| from + skip)
        else {
            break;
        };
        if !word_start || bytes[open] != b'(' {
            continue;
        }
        let (mut depth, mut quoted) = (0_usize, false);
        for (index, byte) in bytes.iter().enumerate().skip(open) {
            match byte {
                b'\'' => quoted = !quoted,
                b'(' if !quoted => depth += 1,
                b')' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(body) = ddl.get(open..=index) {
                            bodies.push(body);
                        }
                        from = index + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    bodies
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

async fn strings(
    manager: &SchemaManager<'_>,
    backend: DatabaseBackend,
    sql: String,
) -> Result<Vec<String>, DbErr> {
    manager
        .get_connection()
        .query_all_raw(Statement::from_string(backend, sql))
        .await?
        .iter()
        .map(|row| row.try_get::<String>("", "v"))
        .collect()
}

fn unsupported(backend: DatabaseBackend) -> DbErr {
    DbErr::Migration(format!("{backend:?} is not a supported backend for {GEAR}"))
}

#[cfg(test)]
#[path = "m0000_products_refuse_a_legacy_or_stale_schema_tests.rs"]
mod tests;

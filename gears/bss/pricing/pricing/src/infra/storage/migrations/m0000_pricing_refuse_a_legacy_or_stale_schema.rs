//! The schema guard (D-423): refuse a legacy or stale pricing schema before anything of the gear
//! is created.
//!
//! The toolkit runner sorts a gear's migrations by name and runs the pending ones, so this one —
//! `m0000_…` — runs before the coordination, broker and outbox migrations (`m0001_…`, `m001_…`)
//! and before `m20260926_000001`. It is pending on every database that predates phase 4 and runs
//! there once. It creates nothing and reads the catalog only: `sqlite_master` and `table_info` on
//! `SQLite`; `information_schema` on Postgres, tables in schema `bss`.
//!
//! What it refuses:
//! - a table of [`LEGACY_TABLES`]: the pre-PriceBook chain's tables that today's chain does not
//!   create (`pricing_plan` and `pricing_price` exist in both and are not evidence);
//! - a shape today's chain replaced in place (D-412) or by renaming: `pricing_price_row`,
//!   `pricing_price` without `price_book_entry_id` (the pre-rename entry), `pricing_reference_op`
//!   without `ref_kind` (before D-412). Sorting first is what makes a pre-rename database meet
//!   this refusal rather than `m20260926_000007`'s "no such column";
//! - the legacy shape of a table name both chains create: `pricing_plan` without `code` (the
//!   legacy revision row). A clean-up that dropped only the tables a refusal named leaves it, and
//!   `m20260926_000010`'s `CREATE TABLE IF NOT EXISTS` would keep it and fail on its `code` index
//!   (phase 4 review F2).
//!
//! A fresh database passes, and so does one migrated by today's chain.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The gear, as the refusal names it.
const GEAR: &str = "bss-pricing";

/// The pre-PriceBook chain's tables minus today's (D-423).
///
/// The chain is `bss/products-backup:gears/bss/pricing/pricing/src/infra/storage/migrations/`
/// (`m20260821_000001` to `m20260921_000050`): 47 `pricing_*` tables, of which today's chain creates
/// `pricing_plan` and `pricing_price` too. Sorted; the fast-tier suite re-derives this list from
/// its own census and a fresh chain and compares.
pub const LEGACY_TABLES: &[&str] = &[
    "pricing_approval",
    "pricing_approval_key",
    "pricing_approval_threshold",
    "pricing_approval_threshold_tombstone",
    "pricing_audit_log",
    "pricing_brand_taxonomy",
    "pricing_bulk_operation",
    "pricing_bulk_row_lock",
    "pricing_bundle",
    "pricing_bundle_component",
    "pricing_bundle_revshare",
    "pricing_bundle_revshare_group",
    "pricing_catalog_version_ref",
    "pricing_charge_line",
    "pricing_charge_line_version",
    "pricing_composite_meter",
    "pricing_customer_group_taxonomy",
    "pricing_draft_window",
    "pricing_gl_code_taxonomy",
    "pricing_group_membership",
    "pricing_idempotency_dedup",
    "pricing_market_price",
    "pricing_migration",
    "pricing_operator_flag",
    "pricing_org_tier_taxonomy",
    "pricing_outbox",
    "pricing_partner_taxonomy",
    "pricing_pin_frontier",
    "pricing_plan_addon_rule",
    "pricing_plan_descriptor_set",
    "pricing_plan_period_floor_cap",
    "pricing_plan_phase",
    "pricing_policy_object",
    "pricing_price_overlay",
    "pricing_price_overlay_line",
    "pricing_price_overlay_line_amount",
    "pricing_price_tier_band",
    "pricing_price_window",
    "pricing_read_model",
    "pricing_region_taxonomy",
    "pricing_repricing_journal",
    "pricing_rounding_policy_taxonomy",
    "pricing_snapshot_provenance",
    "pricing_window_baseline",
    "pricing_window_guard",
];

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
        if tables.iter().any(|t| t == "pricing_price_row") {
            stale.push("table pricing_price_row".to_owned());
        }
        for (table, column) in [
            ("pricing_price", "price_book_entry_id"),
            ("pricing_reference_op", "ref_kind"),
            ("pricing_plan", "code"),
        ] {
            if tables.iter().any(|t| t == table)
                && !columns(manager, table).await?.iter().any(|c| c == column)
            {
                stale.push(format!("{table} without column {column}"));
            }
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

/// D-423's refusal.
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

/// The gear's tables, by name, sorted: `sqlite_master` on `SQLite`, schema `bss` on Postgres.
async fn tables(manager: &SchemaManager<'_>) -> Result<Vec<String>, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 8) = \
             'pricing_'"
        }
        DatabaseBackend::Postgres => {
            "SELECT table_name::text AS v FROM information_schema.tables WHERE table_schema = \
             'bss' AND left(table_name::text, 8) = 'pricing_'"
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

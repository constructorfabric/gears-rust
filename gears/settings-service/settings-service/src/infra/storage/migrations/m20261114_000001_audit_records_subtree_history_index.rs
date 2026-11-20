//! `audit_records` gains the index the subtree history reads by.
//!
//! The history of a setting over a tenant's subtree is one statement:
//!
//! ```sql
//! SELECT … FROM audit_records
//!  WHERE declaration_key = $1
//!    AND (tenant_id IN ($2, …) OR tenant_id IS NULL)
//!  ORDER BY occurred_at DESC, id DESC
//!  LIMIT $n
//! ```
//!
//! with up to the subtree budget of tenants in the list. `idx_audit_scoped`
//! (`declaration_key`, `tenant_id`, `occurred_at DESC`) is the wrong shape for
//! it: its order is by time only inside one tenant, so a plan through it
//! visits every listed tenant's range, gathers every record of the setting in
//! the subtree and sorts them all to return one page — work that grows with
//! the subtree's history, not with the page.
//!
//! `idx_audit_key_recent` (`declaration_key`, `occurred_at DESC`, `id DESC`)
//! holds one setting's records already in the order the statement asks for.
//! The equality on the leading column makes the rest of the key the output
//! order, so the index is meant to serve the statement as a walk of the
//! setting's range from its newest entry, the tenant condition applied as a
//! filter on each row, stopping at the `LIMIT` with nothing to sort. That is
//! the intended plan; it has not been measured on a large `PostgreSQL` table.
//! `id` is in the key so the tiebreak of two
//! records of one instant is read off the index as well. The intended cost is
//! the rows walked until a page is full — the page itself where the subtree holds most
//! of the setting's records, more where it holds few of them, and never more
//! than the one setting's records.
//!
//! `idx_audit_scoped` stays: the single-scope history is an equality on
//! `(declaration_key, tenant_id)` and that index answers it without reading
//! another tenant's row.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(
    elided_lifetimes_in_paths,
    reason = "`MigrationTrait` declares `&SchemaManager` with its lifetime elided; \
              spelled `<'_>` here, `async_trait` binds it early where the trait's \
              is late-bound, and the impl no longer matches (E0195)"
)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Both backends take the index in the same spelling.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_audit_key_recent
                     ON audit_records (declaration_key, occurred_at DESC, id DESC);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_audit_key_recent;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20261114_000001_audit_records_subtree_history_index_tests.rs"]
mod m20261114_000001_audit_records_subtree_history_index_tests;

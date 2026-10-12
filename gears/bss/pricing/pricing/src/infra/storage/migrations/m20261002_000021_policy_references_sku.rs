//! D-514: a usage entry stores the SKU revision its meter was checked on, and a policy row stores
//! only its rating rules.
//!
//! Postgres adds `usage_sku_version` and one CHECK. `SQLite` cannot add a CHECK that names two
//! columns of an existing table (000020's precedent), so it adds the column and enforces the same
//! rule with triggers. The backfill rewrites each stored policy with `quantity_semantics` into a
//! rules-only row, using the Rust digest of `bss-pricing-sdk`, and re-points entries at it. The
//! old row stays. `usage_sku_version` stays null. A frozen acceptance that still embeds
//! `quantity_semantics` refuses the migration by name. `down` is irreversible.
use super::super::entity::{price_book_entry as entry_e, usage_rating_policy as policy_e};
use crate::infra::usage_policy_wire::{self, digest_text};
use bss_pricing_sdk::digest::policy_digest;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, QueryFilter, Set, Statement,
};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const NAME: &str = "m20261002_000021_policy_references_sku";

const PG_COLUMN: &[&str] = &[
    "ALTER TABLE bss.pricing_price_book_entry ADD COLUMN IF NOT EXISTS usage_sku_version bigint",
    "ALTER TABLE bss.pricing_price_book_entry DROP CONSTRAINT IF EXISTS pricing_entry_sku_version",
    "ALTER TABLE bss.pricing_price_book_entry ADD CONSTRAINT pricing_entry_sku_version CHECK (usage_sku_version IS NULL OR (usage_policy_id IS NOT NULL AND usage_sku_version >= 1))",
];

const SQLITE_COLUMN: &[&str] = &[
    "ALTER TABLE pricing_price_book_entry ADD COLUMN usage_sku_version integer",
    "CREATE TRIGGER IF NOT EXISTS pricing_entry_sku_version_insert BEFORE INSERT ON pricing_price_book_entry WHEN NEW.usage_sku_version IS NOT NULL AND (NEW.usage_policy_id IS NULL OR NEW.usage_sku_version < 1) BEGIN SELECT RAISE(ABORT, 'pricing_entry_sku_version'); END",
    "CREATE TRIGGER IF NOT EXISTS pricing_entry_sku_version_update BEFORE UPDATE ON pricing_price_book_entry WHEN NEW.usage_sku_version IS NOT NULL AND (NEW.usage_policy_id IS NULL OR NEW.usage_sku_version < 1) BEGIN SELECT RAISE(ABORT, 'pricing_entry_sku_version'); END",
];

async fn sqlite_has_column(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name AS v FROM pragma_table_info('pricing_price_book_entry') WHERE name = 'usage_sku_version'"
                .to_owned(),
        ))
        .await?;
    Ok(!rows.is_empty())
}

async fn refuse_frozen_acceptance(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Postgres => "SELECT receipt_json AS v FROM bss.pricing_acceptance",
        DatabaseBackend::Sqlite => "SELECT receipt_json AS v FROM pricing_acceptance",
        other => {
            return Err(DbErr::Migration(format!(
                "{other:?} is not a supported backend for bss-pricing"
            )));
        }
    };
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(backend, sql.to_owned()))
        .await
        .map_err(|e| DbErr::Migration(format!("{NAME}: read pricing_acceptance: {e}")))?;
    for row in rows {
        let text: String = row.try_get("", "v").map_err(|e| {
            DbErr::Migration(format!("{NAME}: pricing_acceptance receipt_json: {e}"))
        })?;
        if text.contains("\"quantity_semantics\"") {
            return Err(DbErr::Migration(format!(
                "{NAME}: pricing_acceptance holds a frozen binding that embeds quantity_semantics"
            )));
        }
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the backfill reads and updates every tenant; a migration has no caller scope"
)]
async fn reshape(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    let rows = policy_e::Entity::find()
        .all(conn)
        .await
        .map_err(|e| DbErr::Migration(format!("{NAME}: read policies: {e}")))?;
    for row in rows {
        if row.content.get("quantity_semantics").is_none() {
            continue;
        }
        let wire: usage_policy_wire::UsageRatingPolicyInput =
            serde_json::from_value(row.content.clone()).map_err(|e| {
                DbErr::Migration(format!(
                    "{NAME}: policy {} version {} content: {e}",
                    row.policy_id, row.version
                ))
            })?;
        let digest = digest_text(policy_digest(&(&wire).into()));
        let content = serde_json::to_value(&wire).map_err(|e| {
            DbErr::Migration(format!("{NAME}: policy {} content: {e}", row.policy_id))
        })?;
        let model = policy_e::ActiveModel {
            tenant_id: Set(row.tenant_id),
            policy_id: Set(uuid::Uuid::now_v7()),
            version: Set(1),
            digest: Set(digest.clone()),
            content: Set(content),
            created_at: Set(row.created_at),
            created_by: Set(row.created_by),
        };
        match policy_e::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([policy_e::Column::TenantId, policy_e::Column::Digest])
                    .do_nothing()
                    .to_owned(),
            )
            .exec(conn)
            .await
        {
            Ok(_) | Err(DbErr::RecordNotInserted) => {}
            Err(e) => {
                return Err(DbErr::Migration(format!(
                    "{NAME}: insert rules-only policy: {e}"
                )));
            }
        }
        let stored = policy_e::Entity::find()
            .filter(policy_e::Column::TenantId.eq(row.tenant_id))
            .filter(policy_e::Column::Digest.eq(digest.clone()))
            .one(conn)
            .await
            .map_err(|e| DbErr::Migration(format!("{NAME}: read rules-only policy: {e}")))?
            .ok_or_else(|| {
                DbErr::Migration(format!(
                    "{NAME}: rules-only policy digest {digest} was not stored"
                ))
            })?;
        entry_e::Entity::update_many()
            .col_expr(
                entry_e::Column::UsagePolicyId,
                Expr::value(stored.policy_id),
            )
            .col_expr(
                entry_e::Column::UsagePolicyVersion,
                Expr::value(stored.version),
            )
            .col_expr(
                entry_e::Column::UsagePolicyDigest,
                Expr::value(stored.digest.clone()),
            )
            .filter(entry_e::Column::TenantId.eq(row.tenant_id))
            .filter(entry_e::Column::UsagePolicyId.eq(row.policy_id))
            .filter(entry_e::Column::UsagePolicyVersion.eq(row.version))
            .filter(entry_e::Column::UsagePolicyDigest.eq(row.digest.clone()))
            .exec(conn)
            .await
            .map_err(|e| DbErr::Migration(format!("{NAME}: re-point entries: {e}")))?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        refuse_frozen_acceptance(manager).await?;
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                if !sqlite_has_column(manager).await? {
                    super::exec_backend(self.name(), manager, &[], SQLITE_COLUMN).await?;
                }
            }
            DatabaseBackend::Postgres => {
                super::exec_backend(self.name(), manager, PG_COLUMN, &[]).await?;
            }
            backend => {
                return Err(DbErr::Migration(format!(
                    "{backend:?} is not a supported backend for bss-pricing"
                )));
            }
        }
        reshape(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{NAME}: irreversible — a usage entry keeps the SKU revision its meter was checked on (D-514)"
        )))
    }
}

#[cfg(test)]
#[path = "m20261002_000021_policy_references_sku_tests.rs"]
mod tests;

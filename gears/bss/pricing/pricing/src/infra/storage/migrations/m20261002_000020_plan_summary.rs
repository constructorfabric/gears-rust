//! D-484: time-stable list facts on `pricing_plan`.
//!
//! A stored selling/change column would go stale at midnight, so the columns are facts a write
//! can recompute and the day-dependent axes are derived from them. The backfill reads plans and
//! revisions separately and updates by id: a SQL join on a `SQLite` uuid blob silently updates
//! nothing.
//!
//! Postgres adds the columns and the pairing CHECKs. `SQLite` cannot add a CHECK that names two
//! columns, and rebuilding `pricing_plan` would rebuild `pricing_plan_revision` too (its child),
//! which would rewrite the table 000017 just widened. So `SQLite` adds the columns and enforces
//! the same pairs with triggers. `down` is irreversible.
use super::super::entity::{plan as plan_e, plan_revision as revision_e, price_book as book_e};
use crate::infra::plan_summary::{self, RevisionFact};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QuerySelect, Statement,
};
use sea_orm_migration::prelude::*;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

const NAME: &str = "m20261002_000020_plan_summary";

const PG_COLUMNS: &[&str] = &[
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS work_revision_id uuid",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS work_state text",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS scheduled_revision_id uuid",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS scheduled_from date",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS published_revision_id uuid",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS current_book_id uuid",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS current_currency text",
    "ALTER TABLE bss.pricing_plan ADD COLUMN IF NOT EXISTS last_activity_at timestamptz NOT NULL DEFAULT '1970-01-01T00:00:00Z'",
];

const PG_CHECKS: &[&str] = &[
    "ALTER TABLE bss.pricing_plan DROP CONSTRAINT IF EXISTS pricing_plan_work_pair",
    "ALTER TABLE bss.pricing_plan ADD CONSTRAINT pricing_plan_work_pair CHECK ((work_revision_id IS NULL) = (work_state IS NULL))",
    "ALTER TABLE bss.pricing_plan DROP CONSTRAINT IF EXISTS pricing_plan_work_state",
    "ALTER TABLE bss.pricing_plan ADD CONSTRAINT pricing_plan_work_state CHECK (work_state IS NULL OR work_state IN ('draft','pending'))",
    "ALTER TABLE bss.pricing_plan DROP CONSTRAINT IF EXISTS pricing_plan_scheduled_pair",
    "ALTER TABLE bss.pricing_plan ADD CONSTRAINT pricing_plan_scheduled_pair CHECK ((scheduled_revision_id IS NULL) = (scheduled_from IS NULL))",
];

const SQLITE_UP: &[&str] = &[
    "ALTER TABLE pricing_plan ADD COLUMN work_revision_id text",
    "ALTER TABLE pricing_plan ADD COLUMN work_state text",
    "ALTER TABLE pricing_plan ADD COLUMN scheduled_revision_id text",
    "ALTER TABLE pricing_plan ADD COLUMN scheduled_from text",
    "ALTER TABLE pricing_plan ADD COLUMN published_revision_id text",
    "ALTER TABLE pricing_plan ADD COLUMN current_book_id text",
    "ALTER TABLE pricing_plan ADD COLUMN current_currency text",
    "ALTER TABLE pricing_plan ADD COLUMN last_activity_at text NOT NULL DEFAULT '1970-01-01T00:00:00Z'",
    "CREATE TRIGGER pricing_plan_work_pair_insert BEFORE INSERT ON pricing_plan WHEN (NEW.work_revision_id IS NULL) != (NEW.work_state IS NULL) BEGIN SELECT RAISE(ABORT, 'pricing_plan_work_pair'); END",
    "CREATE TRIGGER pricing_plan_work_pair_update BEFORE UPDATE ON pricing_plan WHEN (NEW.work_revision_id IS NULL) != (NEW.work_state IS NULL) BEGIN SELECT RAISE(ABORT, 'pricing_plan_work_pair'); END",
    "CREATE TRIGGER pricing_plan_work_state_insert BEFORE INSERT ON pricing_plan WHEN NEW.work_state IS NOT NULL AND NEW.work_state NOT IN ('draft','pending') BEGIN SELECT RAISE(ABORT, 'pricing_plan_work_state'); END",
    "CREATE TRIGGER pricing_plan_work_state_update BEFORE UPDATE ON pricing_plan WHEN NEW.work_state IS NOT NULL AND NEW.work_state NOT IN ('draft','pending') BEGIN SELECT RAISE(ABORT, 'pricing_plan_work_state'); END",
    "CREATE TRIGGER pricing_plan_scheduled_pair_insert BEFORE INSERT ON pricing_plan WHEN (NEW.scheduled_revision_id IS NULL) != (NEW.scheduled_from IS NULL) BEGIN SELECT RAISE(ABORT, 'pricing_plan_scheduled_pair'); END",
    "CREATE TRIGGER pricing_plan_scheduled_pair_update BEFORE UPDATE ON pricing_plan WHEN (NEW.scheduled_revision_id IS NULL) != (NEW.scheduled_from IS NULL) BEGIN SELECT RAISE(ABORT, 'pricing_plan_scheduled_pair'); END",
];

async fn sqlite_has_summary(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name AS v FROM pragma_table_info('pricing_plan') WHERE name = 'last_activity_at'"
                .to_owned(),
        ))
        .await?;
    Ok(!rows.is_empty())
}

#[derive(sea_orm::FromQueryResult)]
struct BookCurrency {
    id: Uuid,
    tenant_id: Uuid,
    currency: String,
}

/// The plan columns the backfill reads. A later migration can add a column without breaking this one.
#[derive(sea_orm::FromQueryResult)]
struct PlanStamp {
    id: Uuid,
    tenant_id: Uuid,
    updated_at: time::OffsetDateTime,
}

/// The revision columns the backfill reads. Same reason as [`PlanStamp`].
#[derive(sea_orm::FromQueryResult)]
struct RevisionStamp {
    id: Uuid,
    tenant_id: Uuid,
    plan_id: Uuid,
    book_id: Uuid,
    state: String,
    available_from: Option<time::Date>,
    updated_at: time::OffsetDateTime,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the backfill reads and updates every tenant; a migration has no caller scope"
)]
async fn backfill(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    let plans = plan_e::Entity::find()
        .select_only()
        .column(plan_e::Column::Id)
        .column(plan_e::Column::TenantId)
        .column(plan_e::Column::UpdatedAt)
        .into_model::<PlanStamp>()
        .all(conn)
        .await
        .map_err(|e| DbErr::Migration(format!("{NAME}: read plans: {e}")))?;
    let revisions = revision_e::Entity::find()
        .select_only()
        .column(revision_e::Column::Id)
        .column(revision_e::Column::TenantId)
        .column(revision_e::Column::PlanId)
        .column(revision_e::Column::BookId)
        .column(revision_e::Column::State)
        .column(revision_e::Column::AvailableFrom)
        .column(revision_e::Column::UpdatedAt)
        .into_model::<RevisionStamp>()
        .all(conn)
        .await
        .map_err(|e| DbErr::Migration(format!("{NAME}: read revisions: {e}")))?;
    // id, tenant and currency only: a chain that has not yet added a later book column
    // (the 000015 upgrade applies this migration while `description` is still absent) must
    // still read the currency the summary stores.
    let books = book_e::Entity::find()
        .select_only()
        .column(book_e::Column::Id)
        .column(book_e::Column::TenantId)
        .column(book_e::Column::Currency)
        .into_model::<BookCurrency>()
        .all(conn)
        .await
        .map_err(|e| DbErr::Migration(format!("{NAME}: read books: {e}")))?;
    let currency: HashMap<(Uuid, Uuid), String> = books
        .into_iter()
        .map(|b| ((b.tenant_id, b.id), b.currency))
        .collect();
    let mut by_plan: HashMap<(Uuid, Uuid), Vec<RevisionFact>> = HashMap::new();
    for revision in revisions {
        by_plan
            .entry((revision.tenant_id, revision.plan_id))
            .or_default()
            .push(RevisionFact {
                id: revision.id,
                state: revision.state,
                available_from: revision.available_from,
                updated_at: revision.updated_at,
                book_id: revision.book_id,
                currency: currency
                    .get(&(revision.tenant_id, revision.book_id))
                    .cloned(),
            });
    }
    for plan in plans {
        let facts = by_plan
            .get(&(plan.tenant_id, plan.id))
            .map_or(&[][..], Vec::as_slice);
        let summary = plan_summary::summarize(plan.updated_at, facts)
            .map_err(|e| DbErr::Migration(format!("{NAME}: plan {}: {e}", plan.id)))?;
        // By id, in Rust. A SQL join on a SQLite uuid blob updates nothing.
        let updated = plan_e::Entity::update_many()
            .col_expr(
                plan_e::Column::WorkRevisionId,
                Expr::value(summary.work_revision_id),
            )
            .col_expr(plan_e::Column::WorkState, Expr::value(summary.work_state))
            .col_expr(
                plan_e::Column::ScheduledRevisionId,
                Expr::value(summary.scheduled_revision_id),
            )
            .col_expr(
                plan_e::Column::ScheduledFrom,
                Expr::value(summary.scheduled_from),
            )
            .col_expr(
                plan_e::Column::PublishedRevisionId,
                Expr::value(summary.published_revision_id),
            )
            .col_expr(
                plan_e::Column::CurrentBookId,
                Expr::value(summary.current_book_id),
            )
            .col_expr(
                plan_e::Column::CurrentCurrency,
                Expr::value(summary.current_currency),
            )
            .col_expr(
                plan_e::Column::LastActivityAt,
                Expr::value(summary.last_activity_at),
            )
            .filter(plan_e::Column::Id.eq(plan.id))
            .exec(conn)
            .await
            .map_err(|e| DbErr::Migration(format!("{NAME}: update {}: {e}", plan.id)))?;
        if updated.rows_affected != 1 {
            return Err(DbErr::Migration(format!(
                "{NAME}: plan {} was not updated",
                plan.id
            )));
        }
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                if !sqlite_has_summary(manager).await? {
                    super::exec_backend(self.name(), manager, &[], SQLITE_UP).await?;
                }
            }
            DatabaseBackend::Postgres => {
                super::exec_backend(self.name(), manager, PG_COLUMNS, &[]).await?;
                super::exec_backend(self.name(), manager, PG_CHECKS, &[]).await?;
            }
            backend => {
                return Err(DbErr::Migration(format!(
                    "{backend:?} is not a supported backend for bss-pricing"
                )));
            }
        }
        backfill(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible — the plan summary is filled from the revisions (D-484)",
            self.name()
        )))
    }
}

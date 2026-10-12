//! Recompute one plan's stored summary after a write that matched (D-484).
//!
//! One select (the plan, its revisions and each revision's book currency) and one update of the
//! summary columns only. The plan's `version` and `updated_at` stay the If-Match clock (D-448).
use super::driver_failure;
use crate::infra::plan_summary::{self, RevisionFact};
use crate::infra::storage::{
    RepoError,
    entity::{plan as plan_e, plan_revision as revision_e, price_book as book_e},
};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, JoinType, QuerySelect};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt, SecureUpdateExt};
use uuid::Uuid;

#[derive(sea_orm::FromQueryResult)]
struct SummaryRow {
    id: Uuid,
    plan_updated_at: time::OffsetDateTime,
    revision_id: Option<Uuid>,
    state: Option<String>,
    available_from: Option<time::Date>,
    revision_updated_at: Option<time::OffsetDateTime>,
    book_id: Option<Uuid>,
    currency: Option<String>,
}

/// Which plan to recompute: the id, or the plan that holds a revision.
enum Target {
    Plan(Uuid),
    Revision(Uuid),
}

/// After a write that matched a plan row.
/// # Errors
/// `PLAN_NOT_FOUND` when the plan is not in scope; `CorruptRow` for a summary the columns cannot
/// store; database failures keep their type.
pub async fn refresh(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    plan_id: Uuid,
) -> Result<(), RepoError> {
    apply(runner, scope, tenant, Target::Plan(plan_id)).await
}

/// After a write that matched a revision row. One select: the plan is the revision's.
/// # Errors
/// `PLAN_NOT_FOUND` when the revision's plan is not in scope; `CorruptRow`; database failures.
pub async fn refresh_revision(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revision_id: Uuid,
) -> Result<(), RepoError> {
    apply(runner, scope, tenant, Target::Revision(revision_id)).await
}

async fn apply(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    target: Target,
) -> Result<(), RepoError> {
    let on_revision: sea_orm::RelationDef = plan_e::Entity::belongs_to(revision_e::Entity)
        .from(plan_e::Column::Id)
        .to(revision_e::Column::PlanId)
        .on_condition(move |_, _| Condition::all().add(revision_e::Column::TenantId.eq(tenant)))
        .into();
    let on_book: sea_orm::RelationDef = revision_e::Entity::belongs_to(book_e::Entity)
        .from(revision_e::Column::BookId)
        .to(book_e::Column::Id)
        .on_condition(move |_, _| Condition::all().add(book_e::Column::TenantId.eq(tenant)))
        .into();
    let mut filter = Condition::all().add(plan_e::Column::TenantId.eq(tenant));
    match target {
        Target::Plan(id) => filter = filter.add(plan_e::Column::Id.eq(id)),
        Target::Revision(id) => {
            let plan_of = sea_orm::sea_query::Query::select()
                .column(revision_e::Column::PlanId)
                .from(revision_e::Entity)
                .and_where(revision_e::Column::TenantId.eq(tenant))
                .and_where(revision_e::Column::Id.eq(id))
                .to_owned();
            filter =
                filter.add(Expr::col((plan_e::Entity, plan_e::Column::Id)).in_subquery(plan_of));
        }
    }
    let rows = plan_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter)
        .project_all(runner, |query| {
            query
                .select_only()
                .join(JoinType::LeftJoin, on_revision)
                .join(JoinType::LeftJoin, on_book)
                .column(plan_e::Column::Id)
                .column_as(plan_e::Column::UpdatedAt, "plan_updated_at")
                .column_as(revision_e::Column::Id, "revision_id")
                .column_as(revision_e::Column::State, "state")
                .column_as(revision_e::Column::AvailableFrom, "available_from")
                .column_as(revision_e::Column::UpdatedAt, "revision_updated_at")
                .column_as(revision_e::Column::BookId, "book_id")
                .column_as(book_e::Column::Currency, "currency")
                .into_model::<SummaryRow>()
        })
        .await
        .map_err(|e| driver_failure("read the plan summary".into(), e))?;
    let Some(first) = rows.first() else {
        return Err(RepoError::Conflict {
            code: "PLAN_NOT_FOUND",
        });
    };
    let plan_id = first.id;
    let plan_updated_at = first.plan_updated_at;
    let mut facts = Vec::new();
    for row in &rows {
        let (Some(id), Some(state), Some(updated_at), Some(book_id)) = (
            row.revision_id,
            row.state.clone(),
            row.revision_updated_at,
            row.book_id,
        ) else {
            continue;
        };
        facts.push(RevisionFact {
            id,
            state,
            available_from: row.available_from,
            updated_at,
            book_id,
            currency: row.currency.clone(),
        });
    }
    let summary = plan_summary::summarize(plan_updated_at, &facts)?;
    let updated = plan_e::Entity::update_many()
        .secure()
        .scope_with(scope)
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
        .filter(
            Condition::all()
                .add(plan_e::Column::TenantId.eq(tenant))
                .add(plan_e::Column::Id.eq(plan_id)),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("write the plan summary".into(), e))?;
    if updated.rows_affected != 1 {
        return Err(RepoError::Conflict {
            code: "PLAN_NOT_FOUND",
        });
    }
    Ok(())
}

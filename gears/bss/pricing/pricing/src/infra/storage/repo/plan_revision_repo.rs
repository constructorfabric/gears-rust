//! Scoped plan revision persistence with conditional versions and pending ownership (D-394).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-revision-book:p1
use super::{driver_failure, map_unique, matched};
use crate::domain::plan::RevisionState;
use crate::infra::storage::{RepoError, entity::plan_revision as e};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(e::Column::TenantId.eq(tenant))
        .add(e::Column::Id.eq(id))
}
fn unlocked_draft(tenant: Uuid, id: Uuid, version: i64) -> Condition {
    key(tenant, id)
        .add(e::Column::Version.eq(version))
        .add(e::Column::State.eq(RevisionState::Draft.as_str()))
        .add(e::Column::PendingUnitId.is_null())
}
/// A unique conflict of a revision write. Postgres names the index; `SQLite` names only the
/// columns of a partial index, so its single-column `plan_id` form is whichever of the three
/// partial indexes the written state joins: the published one, the scheduled one (D-446), or the
/// draft-or-pending one.
fn map_revision_unique(context: &str, error: ScopeError, state: &str) -> RepoError {
    if error.is_unique_violation() {
        let message = error.to_string();
        if let Some(code) = super::unique_code(&message) {
            return RepoError::Conflict { code };
        }
        if message.contains("pricing_plan_revision.plan_id") {
            let code = if state == RevisionState::Published.as_str() {
                "REVISION_PUBLISHED_EXISTS"
            } else if state == RevisionState::Scheduled.as_str() {
                "REVISION_SCHEDULED_EXISTS"
            } else {
                "REVISION_DRAFT_EXISTS"
            };
            return RepoError::Conflict { code };
        }
    }
    driver_failure(context.to_owned(), error)
}
async fn book_in_tenant(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    book: Uuid,
) -> Result<(), RepoError> {
    if super::book_repo::find(runner, scope, tenant, book)
        .await?
        .is_none()
    {
        return Err(RepoError::Conflict {
            code: "BOOK_NOT_FOUND",
        });
    }
    Ok(())
}
/// Insert a revision of a tenant's plan on a tenant's book.
/// # Errors
/// `PLAN_NOT_FOUND`, `BOOK_NOT_FOUND`, `REVISION_NO_TAKEN`, `REVISION_DRAFT_EXISTS`,
/// `REVISION_PUBLISHED_EXISTS` or `REVISION_SCHEDULED_EXISTS`; database failures keep their type.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    if super::plan_repo::find(runner, scope, m.tenant_id, m.plan_id)
        .await?
        .is_none()
    {
        return Err(RepoError::Conflict {
            code: "PLAN_NOT_FOUND",
        });
    }
    book_in_tenant(runner, scope, m.tenant_id, m.book_id).await?;
    let state = m.state.clone();
    let active = e::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        plan_id: Set(m.plan_id),
        rev_no: Set(m.rev_no),
        book_id: Set(m.book_id),
        state: Set(m.state),
        available_from: Set(m.available_from),
        pending_unit_id: Set(m.pending_unit_id),
        approved_by_unit_id: Set(m.approved_by_unit_id),
        published_at: Set(m.published_at),
        version: Set(m.version),
        created_by: Set(m.created_by),
        created_at: Set(m.created_at),
        updated_at: Set(m.updated_at),
    };
    let saved = e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert plan revision scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_revision_unique("insert plan revision", e, &state))?;
    super::plan_summary::refresh(runner, scope, m.tenant_id, m.plan_id).await?;
    Ok(saved)
}
/// Read by tenant and identity within the authorized scope.
/// # Errors
/// Returns typed database failures.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find plan revision".into(), e))
}
/// The tenant's revisions among `ids`, in ONE statement (D-428); an id the tenant does not hold
/// has no row.
/// # Errors
/// Returns typed database failures.
pub async fn find_many(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    ids: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::Id.is_in(ids.iter().copied())),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan revisions by id".into(), e))
}
/// The revisions of the tenant's `plans`, by plan and revision number, in ONE statement whatever
/// the number of plans (D-434).
/// # Errors
/// Returns typed database failures.
pub async fn for_plans(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    plans: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if plans.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PlanId.is_in(plans.iter().copied())),
        )
        .order_by(e::Column::PlanId, Order::Asc)
        .order_by(e::Column::RevNo, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list the revisions of plans".into(), e))
}
/// The plans that name one book, a row of [`plans_on_books`]: `plans`, the distinct plans with a
/// draft, pending, scheduled or published revision on it; `named`, the distinct plans with a
/// revision of any state on it. `named - plans` are the plans only superseded revisions keep there.
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct BookPlanCount {
    pub book_id: Uuid,
    pub plans: i64,
    pub named: i64,
}
/// The plans a book is in (D-441): for each of the tenant's `books` a revision of any state names,
/// the distinct plans with a draft, pending, scheduled or published revision whose `book_id` is
/// that book (`plans`: a plan whose only revisions on it are superseded is not in it, and two
/// revisions of one plan count once), and the distinct plans with any revision on it (`named`).
/// ONE grouped statement whatever the number of books and revisions; a book no revision names
/// has no row.
/// The book stats count `plans` and `named - plans` from it, and the book delete (D-444) judges
/// `BOOK_IN_PLAN` by `plans` and `BOOK_IN_PLAN_HISTORY` by `named` from the same read, so a book's
/// stats and its delete never disagree.
/// # Errors
/// Returns typed database failures.
pub async fn plans_on_books(
    runner: &impl DBRunner,
    tenant: Uuid,
    books: &[Uuid],
) -> Result<Vec<BookPlanCount>, RepoError> {
    use sea_orm::QuerySelect;
    use sea_orm::sea_query::Func;
    if books.is_empty() {
        return Ok(Vec::new());
    }
    let plan = || Expr::col((e::Entity, e::Column::PlanId));
    let live = Expr::col((e::Entity, e::Column::State)).ne(RevisionState::Superseded.as_str());
    e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::BookId.is_in(books.iter().copied())),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::BookId)
                .column_as(
                    Expr::from(Func::count_distinct(Expr::case(live, plan()))),
                    "plans",
                )
                .column_as(Expr::from(Func::count_distinct(plan())), "named")
                .group_by(e::Column::BookId)
                .into_model::<BookPlanCount>()
        })
        .await
        .map_err(|e| driver_failure("count the plans on books".into(), e))
}
/// A plan's revisions by revision number.
/// # Errors
/// Returns typed database failures.
pub async fn for_plan(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    plan_id: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PlanId.eq(plan_id)),
        )
        .order_by(e::Column::RevNo, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan revisions".into(), e))
}
/// Change a draft's content (book, availability) at the version the caller read, only while it is
/// an unlocked draft.
/// # Errors
/// `BOOK_NOT_FOUND` for a book outside the tenant; `STALE_REVISION` for a lost version, a locked
/// or a non-draft revision.
pub async fn update_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    book_in_tenant(runner, scope, m.tenant_id, m.book_id).await?;
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::BookId, Expr::value(m.book_id))
        .col_expr(e::Column::AvailableFrom, Expr::value(m.available_from))
        .col_expr(e::Column::UpdatedAt, Expr::value(m.updated_at))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(unlocked_draft(m.tenant_id, m.id, m.version))
        .exec(runner)
        .await
        .map_err(|e| map_unique("update plan revision".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")?;
    super::plan_summary::refresh(runner, scope, m.tenant_id, m.plan_id).await
}
/// Acquire pending ownership only on an unlocked draft at the observed version.
/// # Errors
/// `UNIT_NOT_FOUND` or typed database failures. A lost race returns false.
pub async fn try_lock(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    version: i64,
) -> Result<bool, RepoError> {
    super::unit_exists(runner, scope, tenant, unit, "plan revision unit").await?;
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PendingUnitId, Expr::value(Some(unit)))
        .col_expr(
            e::Column::State,
            Expr::value(RevisionState::Pending.as_str()),
        )
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(unlocked_draft(tenant, id, version))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("lock plan revision conditionally".into(), e))?;
    if result.rows_affected == 1 {
        super::plan_summary::refresh_revision(runner, scope, tenant, id).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}
/// A rejected or withdrawn unit returns its revision to an editable draft.
/// # Errors
/// `STALE_REVISION` when the unit does not own the pending revision.
pub async fn unlock(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PendingUnitId, Expr::value(None::<Uuid>))
        .col_expr(e::Column::State, Expr::value(RevisionState::Draft.as_str()))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::PendingUnitId.eq(unit))
                .add(e::Column::State.eq(RevisionState::Pending.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("unlock plan revision".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")?;
    super::plan_summary::refresh_revision(runner, scope, tenant, id).await
}
/// Publish the revision its unit holds; the lock turns into `approved_by_unit_id`.
/// # Errors
/// `REVISION_NOT_PENDING` when the unit does not hold it; `REVISION_PUBLISHED_EXISTS` while the
/// plan still has a published revision (supersede it first).
pub async fn publish(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::State,
            Expr::value(RevisionState::Published.as_str()),
        )
        .col_expr(e::Column::PendingUnitId, Expr::value(None::<Uuid>))
        .col_expr(e::Column::ApprovedByUnitId, Expr::value(Some(unit)))
        .col_expr(e::Column::PublishedAt, Expr::value(Some(now)))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::PendingUnitId.eq(unit))
                .add(e::Column::State.eq(RevisionState::Pending.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            map_revision_unique(
                "publish plan revision",
                e,
                RevisionState::Published.as_str(),
            )
        })?;
    matched(result.rows_affected, "REVISION_NOT_PENDING")?;
    super::plan_summary::refresh_revision(runner, scope, tenant, id).await
}
/// Supersede a published revision at the version the caller read.
/// # Errors
/// A concurrent change or a revision that is not published is `STALE_REVISION`.
pub async fn supersede(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-4
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::State,
            Expr::value(RevisionState::Superseded.as_str()),
        )
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::State.eq(RevisionState::Published.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("supersede plan revision".into(), e))?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-4
    matched(result.rows_affected, "STALE_REVISION")?;
    super::plan_summary::refresh_revision(runner, scope, tenant, id).await
}
/// The UTC day of `now`: the day a revision's `available_from` is compared with (D-447).
fn utc_day(now: time::OffsetDateTime) -> time::Date {
    now.to_offset(time::UtcOffset::UTC).date()
}
/// A scheduled revision whose sale date has come by `today`. A null `available_from` is never on or
/// before a day, so a scheduled row without a date is never due, as `domain::plan::is_due` reads it.
fn due_on(today: time::Date) -> Condition {
    Condition::all()
        .add(e::Column::State.eq(RevisionState::Scheduled.as_str()))
        .add(e::Column::AvailableFrom.lte(today))
}
/// Schedule the revision its unit holds (D-446, D-448): an approval whose sale date is after the
/// apply's day. The lock turns into `approved_by_unit_id`, the state into `scheduled`, the version
/// moves; `published_at` stays null, and the plan's published revision and `published_rev` do not
/// move. The caller decides that the date is in the future; this write does not read it.
/// # Errors
/// `REVISION_NOT_PENDING` when the unit does not hold the pending revision;
/// `REVISION_SCHEDULED_EXISTS` while the plan has another scheduled revision.
pub async fn schedule(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::State,
            Expr::value(RevisionState::Scheduled.as_str()),
        )
        .col_expr(e::Column::PendingUnitId, Expr::value(None::<Uuid>))
        .col_expr(e::Column::ApprovedByUnitId, Expr::value(Some(unit)))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::PendingUnitId.eq(unit))
                .add(e::Column::State.eq(RevisionState::Pending.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            map_revision_unique(
                "schedule plan revision",
                e,
                RevisionState::Scheduled.as_str(),
            )
        })?;
    matched(result.rows_affected, "REVISION_NOT_PENDING")?;
    super::plan_summary::refresh_revision(runner, scope, tenant, id).await
}
/// What [`switch_due`] switched (D-448): what a `PlanRevisionPublished` for it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switched {
    /// The revision that was published before; `None` for a plan's first publication.
    pub superseded_revision_id: Option<Uuid>,
    pub revision_id: Uuid,
    /// The unit that approved the revision (`approved_by_unit_id`).
    pub unit_id: Uuid,
    pub rev_no: i32,
    pub book_id: Uuid,
}
/// Persist the plan's due switch, in the caller's transaction (D-448): the published revision is
/// superseded, then the due scheduled one (state `scheduled`, `available_from` on or before the UTC
/// day of `now`) is published with `published_at` = 00:00 UTC of its `available_from`, then the
/// plan's `published_rev` is advanced WITHOUT its version or `updated_at` moving
/// (`plan_repo::advance_published`). The two revision rows bump their version and `updated_at` as
/// every write does.
///
/// Every write is conditional on the state it reads, so a switch that lost a race is a no-op:
/// `Some` only when the scheduled-to-published update hit its row, `None` when nothing was due or
/// another writer switched first. A superseded predecessor with no revision published after it is
/// refused rather than committed.
/// # Errors
/// `CorruptRow` for a scheduled revision that names no approving unit (nothing is written);
/// `STALE_REVISION` when the predecessor was superseded but the due revision moved under this
/// transaction; `REVISION_PUBLISHED_EXISTS`; typed database failures.
pub async fn switch_due(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    plan_id: Uuid,
    now: time::OffsetDateTime,
) -> Result<Option<Switched>, RepoError> {
    let today = utc_day(now);
    let of_plan = || {
        Condition::all()
            .add(e::Column::TenantId.eq(tenant))
            .add(e::Column::PlanId.eq(plan_id))
    };
    let Some(due) = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(of_plan().add(due_on(today)))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find the due scheduled revision".into(), e))?
    else {
        return Ok(None);
    };
    let (Some(unit_id), Some(from)) = (due.approved_by_unit_id, due.available_from) else {
        return Err(RepoError::CorruptRow(format!(
            "scheduled plan revision {} names no approving unit",
            due.id
        )));
    };
    let previous = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            of_plan()
                .add(e::Column::State.eq(RevisionState::Published.as_str()))
                .add(e::Column::Id.ne(due.id)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("find the published plan revision".into(), e))?;
    let mut superseded_revision_id = None;
    if let Some(previous) = previous {
        let result = e::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                e::Column::State,
                Expr::value(RevisionState::Superseded.as_str()),
            )
            .col_expr(e::Column::UpdatedAt, Expr::value(now))
            .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
            .filter(
                key(tenant, previous.id)
                    .add(e::Column::State.eq(RevisionState::Published.as_str())),
            )
            .exec(runner)
            .await
            .map_err(|e| driver_failure("supersede the switched plan revision".into(), e))?;
        if result.rows_affected == 1 {
            superseded_revision_id = Some(previous.id);
        }
    }
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::State,
            Expr::value(RevisionState::Published.as_str()),
        )
        .col_expr(
            e::Column::PublishedAt,
            Expr::value(Some(crate::domain::plan::published_from(from))),
        )
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(key(tenant, due.id).add(due_on(today)))
        .exec(runner)
        .await
        .map_err(|e| {
            map_revision_unique(
                "publish the due plan revision",
                e,
                RevisionState::Published.as_str(),
            )
        })?;
    if result.rows_affected != 1 {
        return if superseded_revision_id.is_some() {
            Err(RepoError::Conflict {
                code: "STALE_REVISION",
            })
        } else {
            Ok(None)
        };
    }
    super::plan_repo::advance_published(runner, scope, tenant, plan_id, due.rev_no).await?;
    // Once, after the projection. `advance_published` does not refresh again.
    super::plan_summary::refresh(runner, scope, tenant, plan_id).await?;
    Ok(Some(Switched {
        superseded_revision_id,
        revision_id: due.id,
        unit_id,
        rev_no: due.rev_no,
        book_id: due.book_id,
    }))
}
/// Return a scheduled revision that is not yet due (the UTC day of `now` is before its
/// `available_from`) to an unlocked draft (D-448): `approved_by_unit_id` is cleared, the version
/// moves, and its items stay. The write is conditional on that state, which is its concurrency: no
/// If-Match (plan rev 2 M5). The applied unit stays applied in its history.
/// # Errors
/// `REVISION_NOT_SCHEDULED` for a revision in any other state, or one already due;
/// `REVISION_DRAFT_EXISTS` while the plan has another draft or pending revision.
pub async fn unschedule(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let not_due = Condition::any()
        .add(e::Column::AvailableFrom.is_null())
        .add(e::Column::AvailableFrom.gt(utc_day(now)));
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::State, Expr::value(RevisionState::Draft.as_str()))
        .col_expr(e::Column::ApprovedByUnitId, Expr::value(None::<Uuid>))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::State.eq(RevisionState::Scheduled.as_str()))
                .add(not_due),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            map_revision_unique("unschedule plan revision", e, RevisionState::Draft.as_str())
        })?;
    matched(result.rows_affected, "REVISION_NOT_SCHEDULED")?;
    super::plan_summary::refresh_revision(runner, scope, tenant, id).await
}
/// The switch job's scan (D-448): the due scheduled revisions of EVERY tenant on `today`, by
/// `available_from` then id, at most `limit`. Cross-tenant by design (`AccessScope::allow_all()`,
/// as the reference ticker's scans are); the job then switches each plan in its tenant's scope.
/// # Errors
/// Returns typed database failures.
pub async fn due_scheduled(
    runner: &impl DBRunner,
    today: time::Date,
    limit: u64,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(due_on(today))
        .order_by(e::Column::AvailableFrom, Order::Asc)
        .order_by(e::Column::Id, Order::Asc)
        .limit(limit)
        .all(runner)
        .await
        .map_err(|e| driver_failure("due scheduled plan revisions".into(), e))
}
/// Delete an unlocked draft that has no items left, at its observed version; the caller deletes
/// the items first, with their delete ops (D-414).
/// # Errors
/// A lost version, a locked or non-draft revision, or remaining items are `STALE_REVISION`.
pub async fn delete_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
) -> Result<(), RepoError> {
    use crate::infra::storage::entity::plan_item;
    use toolkit_db::secure::SecureDeleteExt;
    let plan_id = find(runner, scope, tenant, id)
        .await?
        .map(|revision| revision.plan_id);
    let items = sea_orm::sea_query::Query::select()
        .expr(Expr::val(1))
        .from(plan_item::Entity)
        .and_where(plan_item::Column::TenantId.eq(tenant))
        .and_where(plan_item::Column::RevisionId.eq(id))
        .to_owned();
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(unlocked_draft(tenant, id, version).add(Expr::exists(items).not()))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete draft plan revision".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")?;
    if let Some(plan_id) = plan_id {
        super::plan_summary::refresh(runner, scope, tenant, plan_id).await?;
    }
    Ok(())
}

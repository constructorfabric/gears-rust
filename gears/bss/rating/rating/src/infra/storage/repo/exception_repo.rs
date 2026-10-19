//! `bss_rating__exception`: one open row per `(tenant_id, subject_kind, subject_ref, reason_code)`,
//! held by the partial unique index `uq_bss_rating__exception__open_subject`.

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, ExprTrait, FromQueryResult, Order, QuerySelect, Set,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureOnConflict,
    SecureUpdateExt,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::exception::ExceptionRepo;
use crate::domain::exception::{
    ExceptionKey, ExceptionReason, ExceptionRecord, ExceptionResolution, SubjectKind, SubjectRef,
    TenantDiscovery,
};
use crate::infra::storage::entity::exception::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::{corrupt, scope_failure};

const TABLE: &str = "bss_rating__exception";

/// Opens the exception of `key`, or counts one more attempt on the open one, in one statement
/// (`INSERT … ON CONFLICT … WHERE resolved_at IS NULL DO UPDATE`), so two concurrent failures of
/// one subject never open two rows.
///
/// # Errors
///
/// A storage failure, or the key's tenant outside `scope`.
pub async fn upsert_open(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ExceptionKey,
    detail: &str,
    now: OffsetDateTime,
) -> Result<ExceptionRecord, DomainError> {
    let active = ActiveModel {
        exception_id: Set(Uuid::now_v7()),
        tenant_id: Set(key.tenant_id()),
        subject_kind: Set(key.subject_kind().as_str().to_owned()),
        subject_ref: Set(key.subject_ref().to_owned()),
        reason_code: Set(key.reason_code().to_owned()),
        detail: Set(detail.to_owned()),
        first_seen_at: Set(now),
        last_attempt_at: Set(now),
        attempts: Set(1),
        resolved_at: Set(None),
        resolution: Set(None),
    };
    let mut on_conflict = SecureOnConflict::<Entity>::columns([
        Column::TenantId,
        Column::SubjectKind,
        Column::SubjectRef,
        Column::ReasonCode,
    ])
    .value(
        Column::Attempts,
        Expr::col((Entity, Column::Attempts)).add(1),
    )
    .and_then(|c| c.update_columns([Column::LastAttemptAt, Column::Detail]))
    .map_err(|e| scope_failure("record exception", e))?;
    on_conflict
        .inner_mut()
        .target_and_where(Expr::col(Column::ResolvedAt).is_null());
    let row = Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("record exception", e))?
        .on_conflict(on_conflict)
        .exec_with_returning(runner)
        .await
        .map_err(|e| scope_failure("record exception", e))?;
    to_domain(row)
}

/// Resolves the open exception of `key`; `true` when one was open.
///
/// # Errors
///
/// A storage failure.
pub async fn resolve_open(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ExceptionKey,
    resolution: &str,
    now: OffsetDateTime,
) -> Result<bool, DomainError> {
    let result = Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(Column::ResolvedAt, Expr::value(Some(now)))
        .col_expr(Column::Resolution, Expr::value(Some(resolution.to_owned())))
        .filter(key_condition(key).add(Column::ResolvedAt.is_null()))
        .exec(runner)
        .await
        .map_err(|e| scope_failure("resolve exception", e))?;
    Ok(result.rows_affected > 0)
}

/// The open exception of `key`, if any.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn find_open(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ExceptionKey,
) -> Result<Option<ExceptionRecord>, DomainError> {
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key_condition(key).add(Column::ResolvedAt.is_null()))
        .one(runner)
        .await
        .map_err(|e| scope_failure("find open exception", e))?
        .map(to_domain)
        .transpose()
}

/// One keyset page of the open exceptions of `kinds` within `scope`: up to `limit` of them in
/// `exception_id` order, after `after` when given. The id never changes, so a walk over the pages
/// visits every row that stays open during it exactly once.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn list_open(
    runner: &impl DBRunner,
    scope: &AccessScope,
    kinds: &[SubjectKind],
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<ExceptionRecord>, DomainError> {
    let mut condition = open_of(kinds);
    if let Some(after) = after {
        condition = condition.add(Column::ExceptionId.gt(after));
    }
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition)
        .order_by(Column::ExceptionId, Order::Asc)
        .limit(limit)
        .all(runner)
        .await
        .map_err(|e| scope_failure("list open exceptions", e))?
        .into_iter()
        .map(to_domain)
        .collect()
}

#[derive(FromQueryResult)]
struct TenantRow {
    tenant_id: Uuid,
}

/// One keyset page of the tenants that hold open exceptions of `kinds`: up to `limit` of them in
/// `tenant_id` order, after `after` when given.
///
/// The sweeper's tenant discovery (DESIGN §4.8: "each job iterates tenants and binds one tenant's
/// `AccessScope` per batch"): the one read that runs on an unconstrained scope. It is crate-private
/// and takes the sweeper's [`TenantDiscovery`], which only the sweeper can make, and it projects
/// the ownership column alone, never a row's data; every read and write that follows binds the
/// service grant of one discovered tenant.
///
/// # Errors
///
/// A storage failure.
pub(crate) async fn open_tenants(
    runner: &impl DBRunner,
    _discovery: &TenantDiscovery,
    kinds: &[SubjectKind],
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Uuid>, DomainError> {
    let discovery = AccessScope::allow_all();
    let mut condition = open_of(kinds);
    if let Some(after) = after {
        condition = condition.add(Column::TenantId.gt(after));
    }
    let rows = Entity::find()
        .secure()
        .scope_with(&discovery)
        .filter(condition)
        .order_by(Column::TenantId, Order::Asc)
        .project_all(runner, |q| {
            q.select_only()
                .column(Column::TenantId)
                .distinct()
                .limit(limit)
                .into_model::<TenantRow>()
        })
        .await
        .map_err(|e| scope_failure("discover exception tenants", e))?;
    Ok(rows.into_iter().map(|r| r.tenant_id).collect())
}

/// The open exceptions of `kinds`.
fn open_of(kinds: &[SubjectKind]) -> Condition {
    Condition::all()
        .add(Column::ResolvedAt.is_null())
        .add(Column::SubjectKind.is_in(kinds.iter().map(|k| k.as_str())))
}

fn key_condition(key: &ExceptionKey) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id()))
        .add(Column::SubjectKind.eq(key.subject_kind().as_str()))
        .add(Column::SubjectRef.eq(key.subject_ref()))
        .add(Column::ReasonCode.eq(key.reason_code()))
}

fn to_domain(row: Model) -> Result<ExceptionRecord, DomainError> {
    // Each column is read by its own type, so a corrupt value is reported on its own column.
    let subject_kind = SubjectKind::parse(&row.subject_kind)
        .ok_or_else(|| corrupt(TABLE, "subject_kind", &row.subject_kind))?;
    let subject_ref = SubjectRef::parse(row.subject_ref.as_str())
        .map_err(|_| corrupt(TABLE, "subject_ref", &row.subject_ref))?;
    let reason_code = ExceptionReason::parse(row.reason_code.as_str())
        .map_err(|_| corrupt(TABLE, "reason_code", &row.reason_code))?;
    let resolution = match (row.resolved_at, row.resolution) {
        (Some(at), Some(how)) => Some(ExceptionResolution { at, how }),
        (None, None) => None,
        (at, how) => {
            return Err(corrupt(
                TABLE,
                "resolution",
                &format!("resolved_at {at:?} with resolution {how:?}"),
            ));
        }
    };
    Ok(ExceptionRecord {
        exception_id: row.exception_id,
        key: ExceptionKey::new(row.tenant_id, subject_kind, subject_ref, reason_code),
        detail: row.detail,
        first_seen_at: row.first_seen_at,
        last_attempt_at: row.last_attempt_at,
        attempts: row.attempts,
        resolution,
    })
}

/// The `SecureORM` implementation of the domain's [`ExceptionRepo`] port.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrmExceptionRepo;

#[async_trait::async_trait]
impl ExceptionRepo for OrmExceptionRepo {
    async fn upsert_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        detail: &str,
        now: OffsetDateTime,
    ) -> Result<ExceptionRecord, DomainError> {
        upsert_open(tx, scope, key, detail, now).await
    }

    async fn resolve_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &ExceptionKey,
        resolution: &str,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        resolve_open(tx, scope, key, resolution, now).await
    }

    async fn list_open(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        kinds: &[SubjectKind],
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<ExceptionRecord>, DomainError> {
        list_open(tx, scope, kinds, after, limit).await
    }

    async fn open_tenants(
        &self,
        tx: &DbTx<'_>,
        discovery: &TenantDiscovery,
        kinds: &[SubjectKind],
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<Uuid>, DomainError> {
        open_tenants(tx, discovery, kinds, after, limit).await
    }
}

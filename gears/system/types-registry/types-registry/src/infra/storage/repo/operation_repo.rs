//! The `operation` / `operation_item` repository: acceptance, idempotency
//! resolution, and the state moves the worker makes on the way to terminality.

use sea_orm::sea_query::{Expr, Func};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder,
    QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertManyExt, SecureUpdateExt,
    secure_insert,
};
use uuid::Uuid;

use crate::domain::admission::Precondition;
use crate::domain::admission::fingerprint::{RequestFingerprint, ScopeHash};
use crate::domain::ports::{
    ItemSuccess, NewOperation, NewOperationItem, OperationItemRow, OperationRow, RecoveryCursor,
};
use crate::infra::storage::entity::enums::{OperationItemStatus, OperationStatus};
use crate::infra::storage::entity::{operation, operation_item};

/// The multi-row insert budget for `operation_item`, where every row binds 15
/// columns rather than the one parameter per row that `IN_CHUNK`'s
/// `SQLITE_MAX_VARIABLE_NUMBER = 999` reasoning budgets for: 66 × 15 = 990.
const ITEM_INSERT_CHUNK: usize = 66;

/// One stored operation as the domain names it. See `entity_repo::row` for why the
/// mapper sits beside the repository rather than on the entity.
fn operation_row(m: operation::Model) -> Result<OperationRow, ScopeError> {
    let idempotency_scope_hash =
        ScopeHash::from_stored(m.idempotency_scope_hash).map_err(|_| {
            ScopeError::Invalid("stored operation idempotency_scope_hash is not 32 bytes")
        })?;
    let request_fingerprint = RequestFingerprint::from_stored(m.request_fingerprint)
        .map_err(|_| ScopeError::Invalid("stored operation request_fingerprint is not 32 bytes"))?;
    Ok(OperationRow {
        id: m.id,
        kind: m.kind.into(),
        dry_run: m.dry_run,
        plane: m.plane.into(),
        tenant_id: m.tenant_id,
        principal_id: m.principal_id,
        idempotency_key: m.idempotency_key,
        idempotency_scope_hash,
        request_fingerprint,
        status: m.status.into(),
        created_at: m.created_at,
        started_at: m.started_at,
        completed_at: m.completed_at,
    })
}

/// One stored item as the domain names it.
fn operation_item_row(m: operation_item::Model) -> Result<OperationItemRow, ScopeError> {
    let precondition = Precondition::from_stored(m.expected_resource_version).ok_or(
        ScopeError::Invalid("stored operation-item precondition is outside the closed vocabulary"),
    )?;
    Ok(OperationItemRow {
        id: m.id,
        operation_id: m.operation_id,
        item_no: m.item_no,
        gts_id: m.gts_id,
        dry_run: m.dry_run,
        kind: m.kind.into(),
        precondition,
        compat_forced: m.compat_forced,
        status: m.status.into(),
        request_payload: m.request_payload,
        result_revision_no: m.result_revision_no,
        result_resource_version: m.result_resource_version,
        error_payload: m.error_payload,
        created_at: m.created_at,
        started_at: m.started_at,
        completed_at: m.completed_at,
    })
}

pub struct OperationRepo;

impl OperationRepo {
    /// Resolve an `Idempotency-Key` within its scope.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_by_idempotency(
        runner: &impl DBRunner,
        scope: &AccessScope,
        idempotency_scope_hash: &[u8],
        idempotency_key: &str,
    ) -> Result<Option<OperationRow>, ScopeError> {
        operation::Entity::find()
            .filter(
                Condition::all()
                    .add(operation::Column::IdempotencyScopeHash.eq(idempotency_scope_hash))
                    .add(operation::Column::IdempotencyKey.eq(idempotency_key)),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(operation_row)
            .transpose()
    }

    /// Read one operation by its UUID — what the outbox handler and
    /// `GET /operations/{id}` resolve.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_by_id(
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<OperationRow>, ScopeError> {
        operation::Entity::find()
            .filter(operation::Column::Id.eq(id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(operation_row)
            .transpose()
    }

    /// Find pending and running operations for startup recovery.
    pub async fn find_nonterminal_ids(
        runner: &impl DBRunner,
        scope: &AccessScope,
        after: Option<RecoveryCursor>,
        limit: u64,
    ) -> Result<Vec<RecoveryCursor>, ScopeError> {
        let mut filter = Condition::all().add(
            Condition::any()
                .add(operation::Column::Status.eq(OperationStatus::Pending))
                .add(operation::Column::Status.eq(OperationStatus::Running)),
        );
        // Keyset rather than offset: the rows this scan returns stay non-terminal
        // until their admission lands, so an offset would re-read the same page.
        if let Some(cursor) = after {
            filter = filter.add(
                Condition::any()
                    .add(operation::Column::CreatedAt.gt(cursor.created_at))
                    .add(
                        Condition::all()
                            .add(operation::Column::CreatedAt.eq(cursor.created_at))
                            .add(operation::Column::Id.gt(cursor.id)),
                    ),
            );
        }
        operation::Entity::find()
            .filter(filter)
            // Creation order, so recovery re-enqueues in the order the operations
            // were accepted. `id` only breaks ties: it is a client-allocated UUID
            // and carries no time component.
            .order_by(operation::Column::CreatedAt, Order::Asc)
            .order_by(operation::Column::Id, Order::Asc)
            .limit(limit)
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| RecoveryCursor {
                        created_at: row.created_at,
                        id: row.id,
                    })
                    .collect()
            })
    }

    /// Insert acceptance; a duplicate `(idempotency_scope_hash, idempotency_key)`
    /// unique violation serializes concurrent acceptances, as in
    /// [`VersionFamilyRepo::create_or_get`].
    ///
    /// ponytail C5: P0 retains terminal operations (small rows, bounded by request
    /// volume). DESIGN §3.2 adds a sweep using `idx_tr_operation_status`, deleting
    /// completed operations only when no revision pins any item.
    ///
    /// # Errors
    /// Propagates the insert's failure, including the unique violation above.
    pub async fn insert(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewOperation,
    ) -> Result<OperationRow, ScopeError> {
        let am = operation::ActiveModel {
            id: Set(new.id),
            kind: Set(new.kind.into()),
            dry_run: Set(new.dry_run),
            plane: Set(new.plane.into()),
            tenant_id: Set(new.tenant_id),
            principal_id: Set(new.principal_id),
            idempotency_key: Set(new.idempotency_key),
            idempotency_scope_hash: Set(new.idempotency_scope_hash.as_bytes().to_vec()),
            request_fingerprint: Set(new.request_fingerprint.as_bytes().to_vec()),
            status: Set(OperationStatus::Pending),
            created_at: Set(new.now),
            started_at: Set(None),
            completed_at: Set(None),
        };
        operation_row(secure_insert::<operation::Entity>(am, scope, runner).await?)
    }

    /// Insert one operation's items, copying `kind` and `dry_run` from the parent.
    ///
    /// # Errors
    /// Propagates the insert's failure.
    pub async fn insert_items(
        runner: &impl DBRunner,
        scope: &AccessScope,
        parent: &OperationRow,
        items: &[NewOperationItem],
    ) -> Result<(), ScopeError> {
        if items.is_empty() {
            return Ok(());
        }
        let rows: Vec<operation_item::ActiveModel> = items
            .iter()
            .map(|item| operation_item::ActiveModel {
                operation_id: Set(parent.id),
                item_no: Set(item.item_no),
                gts_id: Set(item.gts_id.clone()),
                dry_run: Set(parent.dry_run),
                kind: Set(parent.kind.into()),
                expected_resource_version: Set(item.precondition.stored_value()),
                compat_forced: Set(item.compat_forced),
                status: Set(OperationItemStatus::Pending),
                request_payload: Set(Some(item.request_payload.clone())),
                result_revision_no: Set(None),
                result_resource_version: Set(None),
                error_payload: Set(None),
                created_at: Set(parent.created_at),
                started_at: Set(None),
                completed_at: Set(None),
                ..Default::default()
            })
            .collect();
        for chunk in rows.chunks(ITEM_INSERT_CHUNK) {
            operation_item::Entity::insert_many(chunk.to_vec())
                .secure()
                // `operation_item` carries no security dimension of its own: an
                // item is reachable exactly when its operation is, and that row is
                // scoped. There is nothing per-row to validate.
                .scope_unchecked(scope)?
                .exec(runner)
                .await?;
        }
        Ok(())
    }

    /// One operation's items, in `item_no` order — the per-candidate outcome list
    /// `GET /operations/{id}` returns.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_items(
        runner: &impl DBRunner,
        scope: &AccessScope,
        operation_id: Uuid,
    ) -> Result<Vec<OperationItemRow>, ScopeError> {
        operation_item::Entity::find()
            .filter(operation_item::Column::OperationId.eq(operation_id))
            .order_by(operation_item::Column::ItemNo, Order::Asc)
            .secure()
            .scope_with(scope)
            .all(runner)
            .await?
            .into_iter()
            .map(operation_item_row)
            .collect()
    }

    /// Move `pending` to `running` with `started_at` atomically (`ck_tr_operation_state`).
    /// The `WHERE` guard reports no change on redelivery without resetting the clock.
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn mark_running(
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let result = operation::Entity::update_many()
            .secure()
            .col_expr(
                operation::Column::Status,
                Expr::value(OperationStatus::Running),
            )
            .col_expr(operation::Column::StartedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(operation::Column::Id.eq(id))
                    .add(operation::Column::Status.eq(OperationStatus::Pending)),
            )
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Move an operation to `completed`. `completed` means every item is terminal;
    /// outcomes stay on the items and are not aggregated here (`database.sql`).
    ///
    /// # Errors
    /// Propagates the update's failure.
    /// Terminalize an operation delivery abandoned, from **either** non-terminal
    /// status.
    ///
    /// Separate from [`Self::mark_completed`], which only moves a `running` row:
    /// an operation can be abandoned before its pass ever reached `mark_running`,
    /// and widening `mark_completed` instead would let an ordinary pass complete an
    /// operation it never started. One statement rather than a promote-then-complete
    /// pair, so there is no intermediate state and no second failure point.
    ///
    /// `false` means the row was already terminal — an ordinary concurrent outcome.
    ///
    /// # Errors
    /// Propagates scope validation and database update failures.
    pub async fn mark_abandoned(
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let result = operation::Entity::update_many()
            .secure()
            .col_expr(
                operation::Column::Status,
                Expr::value(OperationStatus::Completed),
            )
            .col_expr(operation::Column::CompletedAt, Expr::value(now))
            // `ck_tr_operation_state` requires a completed row to carry both
            // timestamps, and a `pending` operation has no `started_at`: delivery
            // gave up before its pass ever moved the row. Coalesce rather than
            // overwrite, so an operation that did run keeps the instant it started.
            .col_expr(
                operation::Column::StartedAt,
                Expr::expr(Func::coalesce([
                    Expr::col(operation::Column::StartedAt),
                    Expr::value(now),
                ])),
            )
            .filter(
                Condition::all().add(operation::Column::Id.eq(id)).add(
                    Condition::any()
                        .add(operation::Column::Status.eq(OperationStatus::Pending))
                        .add(operation::Column::Status.eq(OperationStatus::Running)),
                ),
            )
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected > 0)
    }

    pub async fn mark_completed(
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let result = operation::Entity::update_many()
            .secure()
            .col_expr(
                operation::Column::Status,
                Expr::value(OperationStatus::Completed),
            )
            .col_expr(operation::Column::CompletedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(operation::Column::Id.eq(id))
                    .add(operation::Column::Status.eq(OperationStatus::Running)),
            )
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Record changed registration atomically to satisfy `ck_tr_operation_item_state`:
    /// set `succeeded`, drop payload, set both timestamps, revision and resource version.
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn mark_item_succeeded(
        runner: &impl DBRunner,
        scope: &AccessScope,
        item_id: i64,
        outcome: ItemSuccess,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let (revision_no, resource_version) = outcome.columns();
        let result = operation_item::Entity::update_many()
            .secure()
            .col_expr(
                operation_item::Column::Status,
                Expr::value(OperationItemStatus::Succeeded),
            )
            .col_expr(
                operation_item::Column::RequestPayload,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                operation_item::Column::ResultRevisionNo,
                Expr::value(revision_no),
            )
            .col_expr(
                operation_item::Column::ResultResourceVersion,
                Expr::value(resource_version),
            )
            .col_expr(operation_item::Column::StartedAt, Expr::value(now))
            .col_expr(operation_item::Column::CompletedAt, Expr::value(now))
            .filter(non_terminal(item_id))
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Record `unchanged`: unlike [`Self::mark_item_succeeded`], no `result_revision_no`
    /// is allocated (ADR-0005). `ck_tr_operation_item_state` enforces this and
    /// `expected_resource_version >= 1`, excluding creations.
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn mark_item_unchanged(
        runner: &impl DBRunner,
        scope: &AccessScope,
        item_id: i64,
        resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let result = operation_item::Entity::update_many()
            .secure()
            .col_expr(
                operation_item::Column::Status,
                Expr::value(OperationItemStatus::Unchanged),
            )
            .col_expr(
                operation_item::Column::RequestPayload,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                operation_item::Column::ResultResourceVersion,
                Expr::value(Some(resource_version)),
            )
            .col_expr(operation_item::Column::StartedAt, Expr::value(now))
            .col_expr(operation_item::Column::CompletedAt, Expr::value(now))
            .filter(non_terminal(item_id))
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }

    /// Terminalize one item as `failed`, carrying the structured reason.
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn mark_item_failed(
        runner: &impl DBRunner,
        scope: &AccessScope,
        item_id: i64,
        error_payload: String,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        let result = operation_item::Entity::update_many()
            .secure()
            .col_expr(
                operation_item::Column::Status,
                Expr::value(OperationItemStatus::Failed),
            )
            .col_expr(
                operation_item::Column::RequestPayload,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                operation_item::Column::ErrorPayload,
                Expr::value(Some(error_payload)),
            )
            .col_expr(operation_item::Column::StartedAt, Expr::value(now))
            .col_expr(operation_item::Column::CompletedAt, Expr::value(now))
            .filter(non_terminal(item_id))
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }
}

/// Guard non-terminal status so outcomes remain write-once (`database.sql`).
/// Overlapping T21 deliveries or same-`Idempotency-Key` retries must report `false`
/// on losing the guard, never overwrite `succeeded` with `failed` by filtering only on `id`.
fn non_terminal(item_id: i64) -> Condition {
    Condition::all()
        .add(operation_item::Column::Id.eq(item_id))
        .add(
            Condition::any()
                .add(operation_item::Column::Status.eq(OperationItemStatus::Pending))
                .add(operation_item::Column::Status.eq(OperationItemStatus::Running)),
        )
}

//! The `operation` / `operation_item` repository: acceptance, idempotency
//! resolution, and the state moves the worker makes on the way to terminality.

use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertManyExt, SecureUpdateExt,
    secure_insert,
};
use uuid::Uuid;

use super::IN_CHUNK;
use crate::domain::admission::Precondition;
use crate::domain::admission::fingerprint::{RequestFingerprint, ScopeHash};
use crate::domain::ports::{NewOperation, NewOperationItem, OperationItemRow, OperationRow};
use crate::infra::storage::entity::enums::{OperationItemStatus, OperationStatus};
use crate::infra::storage::entity::{operation, operation_item};

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

    /// Insert an accepted operation.
    ///
    /// A duplicate `(idempotency_scope_hash, idempotency_key)` surfaces as a
    /// unique violation, which the caller reads as the serialization point between
    /// two concurrent acceptances rather than as a fault — the same protocol
    /// [`VersionFamilyRepo::create_or_get`] uses, and for the same reason: the
    /// constraint is the only thing that can decide a race this layer cannot lock.
    ///
    /// ponytail: ceiling C5 — there is no operation-retention sweep in P0, so
    /// terminal operations accumulate here. Rows are small and bounded by request
    /// volume rather than by entity count. Upgrade path: the DESIGN §3.2 sweep,
    /// which deletes a completed operation only when no revision pins any of its
    /// items — `idx_tr_operation_status` exists for exactly that query.
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
        for chunk in rows.chunks(IN_CHUNK) {
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

    /// Move an operation from `pending` to `running`.
    ///
    /// `ck_tr_operation_state` requires `started_at` at `running`, so both move in
    /// one statement. The `pending` precondition is in the `WHERE`, so a
    /// redelivered message that finds the operation already running affects no row
    /// and is reported as such rather than resetting the clock.
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

    /// Record a committed, changed registration on one item.
    ///
    /// Every column `ck_tr_operation_item_state` couples to `succeeded` moves in
    /// one statement: the payload is dropped, both timestamps are set, and the
    /// revision and resource version are recorded. Splitting them would leave a
    /// row the CHECK rejects halfway.
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn mark_item_succeeded(
        runner: &impl DBRunner,
        scope: &AccessScope,
        item_id: i64,
        revision_no: i32,
        resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
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
                Expr::value(Some(revision_no)),
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

/// One item, and only while it is still non-terminal.
///
/// The status half is the guard: an item's outcome is written once and stands
/// (`database.sql`). Two passes over one operation can overlap — at-least-once
/// delivery (T21), or a retry under the same `Idempotency-Key` arriving while the
/// first pass is still admitting — and filtering on `id` alone let the loser
/// overwrite a `succeeded` item with `failed`. Both writers now report `false`
/// instead, which is what their callers act on.
fn non_terminal(item_id: i64) -> Condition {
    Condition::all()
        .add(operation_item::Column::Id.eq(item_id))
        .add(
            Condition::any()
                .add(operation_item::Column::Status.eq(OperationItemStatus::Pending))
                .add(operation_item::Column::Status.eq(OperationItemStatus::Running)),
        )
}

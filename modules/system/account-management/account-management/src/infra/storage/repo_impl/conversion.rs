//! `SeaORM`-backed implementation of [`ConversionRepo`].
//!
//! Mirrors the conventions established by the sibling [`TenantRepoImpl`]
//! splits ([`reads`], [`lifecycle`], [`retention`]): every method on the
//! [`ConversionRepo`] trait is dispatched to a free function in this
//! module, all reads / mutations go through `SecureORM` against the
//! `conversion_requests` entity. The entity declares
//! `Scopable(tenant_col = "tenant_id", resource_col = "id", no_owner,
//! no_type)`, so a caller-bound `AccessScope` compiles into a real
//! DB-level clamp on `tenant_id` (and / or `id` once a future PDP
//! emits `InTenantSubtree(RESOURCE_ID, …)`); the
//! [`crate::domain::conversion::service::ConversionService`] builds
//! the right shape per caller side (`for_tenant(child_id)` for child-
//! side, `InTenantSubtree(OWNER_TENANT_ID, parent_id, respect_barriers
//! = false)` for parent-side counterparty / inbound listings, with
//! barrier penetration because the dual-consent flows must surface
//! conversions targeting self-managed children whose closure barrier
//! is `1`). INSERT paths call `scope_unchecked` since Scopable INSERT-
//! time clamps are not the right model for inserts. DB errors flow
//! through the canonical-mapping classifier so domain code never sees
//! a raw `DbErr`.
//!
//! Two repo-specific behaviours are pinned here:
//!
//! * `insert_pending` detects the partial-unique violation on
//!   `ux_conversion_requests_pending` (single pending row per tenant)
//!   via `is_unique_violation` and re-reads the existing pending row to
//!   surface [`DomainError::PendingExists`] with the existing request
//!   id. Other DB errors funnel through `map_scope_err` /
//!   `classify_db_err_to_domain` like the tenant repo.
//! * Each `transition_pending_to_*` is a single guarded `UPDATE …
//!   WHERE id = ? AND status = pending AND deleted_at IS NULL`. On
//!   `rows_affected == 0` the impl re-reads the row to distinguish
//!   [`DomainError::NotFound`] from [`DomainError::AlreadyResolved`]
//!   per the trait contract.
//!
//! [`reads`]: crate::infra::storage::repo_impl::reads
//! [`lifecycle`]: crate::infra::storage::repo_impl::lifecycle
//! [`retention`]: crate::infra::storage::repo_impl::retention
//! [`TenantRepoImpl`]: crate::infra::storage::repo_impl::TenantRepoImpl
//! [`ConversionRepo`]: crate::domain::conversion::repo::ConversionRepo

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use modkit_db::secure::{
    DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt, is_unique_violation,
};
use modkit_security::AccessScope;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, Order, QueryFilter};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::conversion::model::{
    ConversionPagination, ConversionRequest, ConversionSide, ConversionStatus,
    NewConversionRequest, TargetMode,
};
use crate::domain::conversion::repo::{ApplyConversionApprovalInput, ConversionRepo};
use crate::domain::error::DomainError;
use crate::domain::tenant::model::TenantStatus;
use crate::infra::storage::entity::{conversion_requests, tenant_closure, tenants};

use super::AmDbProvider;
use super::helpers::{TxError, map_scope_err, map_scope_to_tx, with_serializable_retry};

/// `SeaORM` repository adapter for [`ConversionRepo`].
///
/// Decision rule: separate struct from [`TenantRepoImpl`] because
/// `ConversionRepo` is a disjoint trait — the trait surfaces are
/// independent and sharing storage of [`AmDbProvider`] is enough; no
/// shared state would benefit from a single adapter struct.
///
/// The repo carries no domain-rule dependencies. Type compatibility
/// is enforced by `ConversionService::approve` BEFORE the apply TX
/// opens; the repo only honours the TX-side TOCTOU guard on
/// `tenants.tenant_type_uuid` declared on
/// [`crate::domain::conversion::repo::ApplyConversionApprovalInput`].
pub struct ConversionRepoImpl {
    db: Arc<AmDbProvider>,
}

impl ConversionRepoImpl {
    /// Build a new repo adapter over the shared AM DB provider.
    #[must_use]
    pub const fn new(db: Arc<AmDbProvider>) -> Self {
        Self { db }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Lift a [`conversion_requests::Model`] row into the domain
/// [`ConversionRequest`]. Translates the `SMALLINT`-encoded enums into
/// their typed forms; out-of-domain encodings surface as
/// [`DomainError::Internal`] (the column-level `CHECK` constraint
/// declared by `m0004` already prevents invalid writes, so a violation
/// here implies schema-vs-domain drift).
fn entity_to_conversion(row: conversion_requests::Model) -> Result<ConversionRequest, DomainError> {
    let status =
        ConversionStatus::from_smallint(row.status).ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion_requests.status out-of-domain value: {}",
                row.status
            ),
            cause: None,
        })?;
    let initiator_side =
        ConversionSide::from_smallint(row.initiator_side).ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion_requests.initiator_side out-of-domain value: {}",
                row.initiator_side
            ),
            cause: None,
        })?;
    let target_mode =
        TargetMode::from_smallint(row.target_mode).ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion_requests.target_mode out-of-domain value: {}",
                row.target_mode
            ),
            cause: None,
        })?;
    Ok(ConversionRequest {
        id: row.id,
        tenant_id: row.tenant_id,
        parent_id: row.parent_id,
        child_tenant_name: row.child_tenant_name,
        initiator_side,
        target_mode,
        status,
        requested_by: row.requested_by,
        approved_by: row.approved_by,
        cancelled_by: row.cancelled_by,
        rejected_by: row.rejected_by,
        requested_at: row.requested_at,
        resolved_at: row.resolved_at,
        expires_at: row.expires_at,
        deleted_at: row.deleted_at,
    })
}

/// Build a `Condition` matching a conversion-request row by id while
/// excluding soft-deleted rows. Used by the read paths and the re-read
/// step that follows every guarded transition UPDATE.
fn id_eq_alive(id: Uuid) -> Condition {
    Condition::all()
        .add(conversion_requests::Column::Id.eq(id))
        .add(conversion_requests::Column::DeletedAt.is_null())
}

// ---------------------------------------------------------------------------
// Free functions implementing each ConversionRepo method.
// ---------------------------------------------------------------------------

async fn insert_pending(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    new: &NewConversionRequest,
) -> Result<ConversionRequest, DomainError> {
    // `requested_at` is supplied by the caller (the service layer's
    // `now_fn` clock seam — see `NewConversionRequest`), not read from a
    // wall-clock here, so the repo stays deterministic and unit tests
    // can pin the column value exactly.
    let am = conversion_requests::ActiveModel {
        id: ActiveValue::Set(new.id),
        tenant_id: ActiveValue::Set(new.tenant_id),
        parent_id: ActiveValue::Set(new.parent_id),
        child_tenant_name: ActiveValue::Set(new.child_tenant_name.clone()),
        initiator_side: ActiveValue::Set(new.initiator_side.as_smallint()),
        target_mode: ActiveValue::Set(new.target_mode.as_smallint()),
        status: ActiveValue::Set(ConversionStatus::Pending.as_smallint()),
        requested_by: ActiveValue::Set(new.requested_by),
        approved_by: ActiveValue::Set(None),
        cancelled_by: ActiveValue::Set(None),
        rejected_by: ActiveValue::Set(None),
        requested_at: ActiveValue::Set(new.requested_at),
        resolved_at: ActiveValue::Set(None),
        expires_at: ActiveValue::Set(new.expires_at),
        deleted_at: ActiveValue::Set(None),
    };
    let conn = repo.db.conn()?;
    let insert_res = conversion_requests::Entity::insert(am)
        .secure()
        // INSERT path: `scope_unchecked` per the entity-level contract
        // (Scopable INSERT-time clamps are not the right model — the
        // row is being created and cannot yet be filtered against the
        // caller's scope). Authorization for the operation as a whole
        // is enforced upstream at the service-layer dual-consent role
        // check on the caller-supplied `ConversionCaller`. Mirrors the
        // `tenants` INSERT posture in
        // `repo_impl::lifecycle::insert_provisioning`.
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec_with_returning(&conn)
        .await;

    match insert_res {
        Ok(model) => entity_to_conversion(model),
        Err(modkit_db::secure::ScopeError::Db(db_err)) if is_unique_violation(&db_err) => {
            // Partial-unique-index violation on
            // `ux_conversion_requests_pending` — re-read the existing
            // pending row to surface [`DomainError::PendingExists`]
            // with the conflicting request id. The classifier in
            // `is_unique_violation` is engine-agnostic and may
            // theoretically match other unique constraints on this
            // table; today the partial-unique on `(tenant_id) WHERE
            // status = pending AND deleted_at IS NULL` is the only
            // unique index defined on the table besides the PK. A
            // PK collision (caller-supplied duplicate `request_id`)
            // is impossible by construction — the service layer
            // generates the id at request time — but the post-lookup
            // fall-through to `Internal` below covers it should the
            // contract change.
            let existing = conversion_requests::Entity::find()
                .secure()
                // Read uses `allow_all` for the same reason the INSERT
                // uses `scope_unchecked`: the entity is declared
                // `no_tenant`/`no_resource`, so narrowing has no
                // resolvable property and would silently mask the
                // conflicting row.
                .scope_with(&AccessScope::allow_all())
                .filter(
                    Condition::all()
                        .add(conversion_requests::Column::TenantId.eq(new.tenant_id))
                        .add(
                            conversion_requests::Column::Status
                                .eq(ConversionStatus::Pending.as_smallint()),
                        )
                        .add(conversion_requests::Column::DeletedAt.is_null()),
                )
                .one(&conn)
                .await
                .map_err(map_scope_err)?;
            match existing {
                Some(row) => Err(DomainError::PendingExists {
                    request_id: row.id.to_string(),
                }),
                None => Err(DomainError::Internal {
                    diagnostic: format!(
                        "insert_pending hit a unique violation on tenant {} but no pending row \
                         was visible on re-read (likely a non-pending unique constraint, e.g. \
                         duplicate request_id PK)",
                        new.tenant_id
                    ),
                    // Preserve the SQLSTATE chain for operator triage
                    // of a real PK collision — without it, the
                    // post-lookup fall-through arm has no upstream
                    // diagnostic to inspect, asymmetric with
                    // `map_scope_err` which preserves the cause on
                    // every other DB error path.
                    cause: Some(Box::new(db_err)),
                }),
            }
        }
        Err(other) => Err(map_scope_err(other)),
    }
}

async fn find_by_id(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Option<ConversionRequest>, DomainError> {
    let conn = repo.db.conn()?;
    let row = conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(id_eq_alive(id))
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    match row {
        Some(r) => Ok(Some(entity_to_conversion(r)?)),
        None => Ok(None),
    }
}

async fn find_pending_for_tenant(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Option<ConversionRequest>, DomainError> {
    // `conversion_requests` declares `tenant_col = "tenant_id"` +
    // `resource_col = "id"`, so a narrowed scope from the caller
    // (e.g. `for_tenant(tenant_id)`) compiles to a real DB-level
    // clamp. The caller is responsible for passing a scope that
    // covers `tenant_id`; here we forward it verbatim so the repo
    // does not paper over a mis-routed caller.
    let conn = repo.db.conn()?;
    let row = conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(conversion_requests::Column::TenantId.eq(tenant_id))
                .add(
                    conversion_requests::Column::Status.eq(ConversionStatus::Pending.as_smallint()),
                )
                .add(conversion_requests::Column::DeletedAt.is_null()),
        )
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    match row {
        Some(r) => Ok(Some(entity_to_conversion(r)?)),
        None => Ok(None),
    }
}

/// Shared body for the four guarded `transition_pending_to_*` methods.
///
/// Runs a single `UPDATE … WHERE id = ? AND status = pending AND
/// deleted_at IS NULL` with the caller-supplied column patches. On
/// `rows_affected == 0` re-reads the row to distinguish
/// [`DomainError::NotFound`] from [`DomainError::AlreadyResolved`] per
/// the trait contract; on `rows_affected == 1` re-reads the row to
/// return the post-transition snapshot to the caller.
async fn run_guarded_transition<F>(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    request_id: Uuid,
    new_status: ConversionStatus,
    apply_columns: F,
) -> Result<ConversionRequest, DomainError>
where
    F: FnOnce(
        sea_orm::UpdateMany<conversion_requests::Entity>,
    ) -> sea_orm::UpdateMany<conversion_requests::Entity>,
{
    let conn = repo.db.conn()?;
    let mut update = conversion_requests::Entity::update_many().col_expr(
        conversion_requests::Column::Status,
        Expr::value(new_status.as_smallint()),
    );
    update = apply_columns(update);
    let res = update
        .filter(
            Condition::all()
                .add(conversion_requests::Column::Id.eq(request_id))
                .add(
                    conversion_requests::Column::Status.eq(ConversionStatus::Pending.as_smallint()),
                )
                .add(conversion_requests::Column::DeletedAt.is_null()),
        )
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;

    if res.rows_affected == 0 {
        // The fence rejected this UPDATE: either the row does not
        // exist (NotFound) or it has already left `pending`
        // (AlreadyResolved). Re-read once to disambiguate; the same
        // tx-less pattern as the tenant repo's PATCH path because
        // the trait contract documents the two outcomes as caller-
        // observable distinct errors.
        let existing = conversion_requests::Entity::find()
            .secure()
            // Re-read with `allow_all` so a narrowed caller scope
            // cannot turn `AlreadyResolved` into a misleading
            // `NotFound`; mirrors the rationale in the
            // `is_unique_violation` re-read above. Permanent posture:
            // the entity is `no_tenant, no_resource` and the read is
            // a diagnostic disambiguation on a row we already touched.
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(conversion_requests::Column::Id.eq(request_id))
                    .add(conversion_requests::Column::DeletedAt.is_null()),
            )
            .one(&conn)
            .await
            .map_err(map_scope_err)?;
        return match existing {
            Some(_) => Err(DomainError::AlreadyResolved),
            None => Err(DomainError::ConversionRequestNotFound {
                detail: format!("conversion request {request_id} not found"),
                resource: request_id.to_string(),
            }),
        };
    }

    // Re-read the post-transition row to surface the new column
    // values (status, resolved_at, *_by). The fence above guarantees
    // exactly one row was updated, so the `Option::None` branch here
    // is theoretically unreachable; surface it as `Internal` if the
    // row vanished between UPDATE and SELECT (would only happen if
    // the FK ON DELETE CASCADE fired in the gap, e.g. parent tenant
    // hard-deleted concurrently — operator-action territory).
    let fresh = conversion_requests::Entity::find()
        .secure()
        // Post-transition re-read uses `allow_all` so a narrowed
        // caller scope cannot mask a row we just successfully
        // updated. Same posture as the unique-violation re-read above.
        .scope_with(&AccessScope::allow_all())
        .filter(id_eq_alive(request_id))
        .one(&conn)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::Internal {
            diagnostic: format!(
                "conversion request {request_id} disappeared after successful transition \
                 (likely a concurrent FK cascade from tenant hard-delete)"
            ),
            cause: None,
        })?;
    entity_to_conversion(fresh)
}

async fn __transition_pending_to_approved_test_only(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    request_id: Uuid,
    approved_by: Uuid,
    resolved_at: OffsetDateTime,
) -> Result<ConversionRequest, DomainError> {
    run_guarded_transition(
        repo,
        scope,
        request_id,
        ConversionStatus::Approved,
        move |q| {
            q.col_expr(
                conversion_requests::Column::ApprovedBy,
                Expr::value(Some(approved_by)),
            )
            .col_expr(
                conversion_requests::Column::ResolvedAt,
                Expr::value(Some(resolved_at)),
            )
        },
    )
    .await
}

async fn transition_pending_to_cancelled(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    request_id: Uuid,
    cancelled_by: Uuid,
    resolved_at: OffsetDateTime,
) -> Result<ConversionRequest, DomainError> {
    run_guarded_transition(
        repo,
        scope,
        request_id,
        ConversionStatus::Cancelled,
        move |q| {
            q.col_expr(
                conversion_requests::Column::CancelledBy,
                Expr::value(Some(cancelled_by)),
            )
            .col_expr(
                conversion_requests::Column::ResolvedAt,
                Expr::value(Some(resolved_at)),
            )
        },
    )
    .await
}

async fn transition_pending_to_rejected(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    request_id: Uuid,
    rejected_by: Uuid,
    resolved_at: OffsetDateTime,
) -> Result<ConversionRequest, DomainError> {
    run_guarded_transition(
        repo,
        scope,
        request_id,
        ConversionStatus::Rejected,
        move |q| {
            q.col_expr(
                conversion_requests::Column::RejectedBy,
                Expr::value(Some(rejected_by)),
            )
            .col_expr(
                conversion_requests::Column::ResolvedAt,
                Expr::value(Some(resolved_at)),
            )
        },
    )
    .await
}

async fn transition_pending_to_expired(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    request_id: Uuid,
    resolved_at: OffsetDateTime,
) -> Result<ConversionRequest, DomainError> {
    run_guarded_transition(
        repo,
        scope,
        request_id,
        ConversionStatus::Expired,
        move |q| {
            q.col_expr(
                conversion_requests::Column::ResolvedAt,
                Expr::value(Some(resolved_at)),
            )
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Apply-conversion-approval — single-TX dual-consent apply.
// ---------------------------------------------------------------------------

/// Atomic dual-consent apply driver. Owns the ONE transaction that
/// re-loads the pending row, the converting tenant, and the parent
/// tenant; runs the GTS type re-evaluation BEFORE flipping any tenant
/// flag; flips `tenants.self_managed`; rewrites the barrier on every
/// affected `tenant_closure` row; and stamps the request transition
/// to `Approved`. Wrapped by [`with_serializable_retry`] so the entire
/// recompute survives `40001` contention without leaking partial
/// state.
// @cpt-begin:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-impl
// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-impl
// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-barrier-rematerialization-consistency:p1:inst-dod-barrier-rematerialization
// @cpt-begin:cpt-cf-account-management-dod-managed-self-managed-modes-mixed-mode-tree-consistency:p1:inst-dod-mixed-mode-tree-consistency-impl
#[allow(
    clippy::too_many_lines,
    reason = "single-TX dual-consent apply is the load-bearing seam; splitting it would obscure the strict step ordering (re-load -> type re-eval -> flip -> closure rewrite -> request stamp) the apply algorithm pins"
)]
async fn apply_conversion_approval(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    input: ApplyConversionApprovalInput,
) -> Result<ConversionRequest, DomainError> {
    // Service-trust boundary: the repo intentionally does NOT
    // cross-check `approver_uuid` against an actor registry (see
    // the TODO on `ApplyConversionApprovalInput::approver_uuid`).
    // We still fail closed on `Uuid::nil()` — the most common
    // service-layer bug (default-constructed actor) — so a buggy
    // handler cannot persist a nil-actor approval and corrupt the
    // audit trail. A `debug_assert!` would disappear in release
    // builds; a domain error returned before the retry loop is
    // the production-correct guard.
    // Tracking: cyberfabric-core#1813-followup.
    if input.approver_uuid.is_nil() {
        return Err(DomainError::internal(
            "apply_conversion_approval: approver_uuid MUST NOT be Uuid::nil() (service-layer bug)",
        ));
    }
    let scope_owned = scope.clone();
    with_serializable_retry(&repo.db, move || {
        let scope = scope_owned.clone();
        Box::new(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                // 1. Re-load the pending row inside the TX. SI / SSI
                //    anti-dependency anchor — peer writes that flip
                //    the row out from under us surface as `40001`.
                let req_row = conversion_requests::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(
                        Condition::all()
                            .add(conversion_requests::Column::Id.eq(input.request_id))
                            .add(conversion_requests::Column::DeletedAt.is_null()),
                    )
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| {
                        TxError::Domain(DomainError::ConversionRequestNotFound {
                            detail: format!("conversion request {} not found", input.request_id),
                            resource: input.request_id.to_string(),
                        })
                    })?;
                if req_row.status != ConversionStatus::Pending.as_smallint() {
                    return Err(TxError::Domain(DomainError::AlreadyResolved));
                }

                // Cross-check the caller-supplied `input` against the
                // freshly-reloaded request row. The trait doc on
                // `apply_conversion_approval` documents the input as
                // narrowing-only and not trusted; this is the
                // load-bearing fence that enforces the contract.
                // Mismatched `target_tenant_id` would have us flip
                // `self_managed` and rewrite the closure barrier on
                // the WRONG tenant, while only stamping the request
                // transition. Mismatched `target_mode` would persist
                // a `tenants.self_managed` value the request did not
                // request. Either is a caller bug (today the service
                // path passes matching values), surface as
                // `Internal` so the bug is loud.
                if req_row.tenant_id != input.target_tenant_id {
                    return Err(TxError::Domain(DomainError::Internal {
                        diagnostic: format!(
                            "apply_conversion_approval: input.target_tenant_id ({}) \
                             does not match the reloaded request row's tenant_id ({}); \
                             caller MUST pass values matching the row",
                            input.target_tenant_id, req_row.tenant_id
                        ),
                        cause: None,
                    }));
                }
                if req_row.target_mode != input.target_mode.as_smallint() {
                    return Err(TxError::Domain(DomainError::Internal {
                        diagnostic: format!(
                            "apply_conversion_approval: input.target_mode ({}) does not \
                             match the reloaded request row's target_mode ({}); caller \
                             MUST pass values matching the row",
                            input.target_mode.as_smallint(),
                            req_row.target_mode
                        ),
                        cause: None,
                    }));
                }

                let parent_id_from_req = req_row.parent_id.ok_or_else(|| {
                    TxError::Domain(DomainError::Internal {
                        diagnostic: format!(
                            "conversion {}: parent_id missing on pending row; root-tenant guard \
                             should have rejected this earlier",
                            input.request_id
                        ),
                        cause: None,
                    })
                })?;

                // 2. Re-load the converting tenant + status precondition.
                //
                // No `deleted_at IS NULL` filter here: a tenant that
                // was `Active` at the service-level precheck and got
                // soft-deleted before this TX opens still has a row
                // (with `status = Deleted` and `deleted_at` set), and
                // the documented contract says the failure surfaces
                // as `Validation` (inactive tenant), NOT `NotFound`.
                // The status check below catches that case. The only
                // path where this reload returns `None` is when the
                // tenant was hard-deleted between request and approve
                // — the FK cascade would have wiped the conversion
                // row too, so we wouldn't have reached this point.
                //
                // `allow_all` (not the forwarded `scope`) is load-
                // bearing here: the forwarded `scope` is the
                // conversion-row clamp built by
                // [`ConversionService::conversion_repo_scope`] —
                // shaped for the `conversion_requests` entity
                // (`OWNER_TENANT_ID` on `tenant_id`). `tenants`
                // declares `resource_col = "id"` only, so an
                // `OWNER_TENANT_ID`-shaped filter does not resolve on
                // `tenants` and the secure-extension would fail-
                // closed — silently turning the load into a
                // `WHERE false` and surfacing `NotFound` on a row that
                // exists. The service-layer caller-visibility fence on
                // the parent ([`require_caller_tenant_visible`]) has
                // already authorized the caller's identity; the
                // converting child for a parent-side approval may sit
                // behind the closure barrier and be invisible to a
                // parent-narrowed `tenants` scope anyway, so the
                // structural reload uses `allow_all` to mirror the
                // parent-row reload below.
                let tenant_row = tenants::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(Condition::all().add(tenants::Column::Id.eq(input.target_tenant_id)))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| {
                        TxError::Domain(DomainError::NotFound {
                            detail: format!("tenant {} not found", input.target_tenant_id),
                            resource: input.target_tenant_id.to_string(),
                        })
                    })?;
                if tenant_row.status != TenantStatus::Active.as_smallint() {
                    // Decode the raw SMALLINT into the canonical
                    // lowercase token (`provisioning` / `suspended` /
                    // `deleted` / ...) so a tenant that flips inactive
                    // between the service precheck and the TX recheck
                    // surfaces with the same string the rest of the
                    // service's validation errors use, instead of the
                    // engine-internal `status=2`. Any unrecognised
                    // value (which would indicate a corrupt row, not a
                    // race) falls back to the raw integer for triage.
                    let observed = TenantStatus::from_smallint(tenant_row.status)
                        .map_or_else(|| tenant_row.status.to_string(), |s| s.as_str().to_owned());
                    return Err(TxError::Domain(DomainError::Validation {
                        detail: format!(
                            "tenant {} is not active (status={observed})",
                            tenant_row.id,
                        ),
                    }));
                }
                // TOCTOU guard for the service-layer type compatibility
                // check. `ConversionService::approve` runs the
                // `allowed_parent_types` barrier on the snapshot of
                // both tenants it loads outside this TX; the values it
                // observed flow in via
                // `expected_tenant_type_uuid` / `expected_parent_tenant_type_uuid`.
                // A peer that retyped this tenant between the service's
                // check and the TX would otherwise leave us approving
                // against a stale pairing. Mismatched type surfaces as
                // `Validation` so the conversion request stays
                // recoverable — operator retries approve, the service
                // re-runs the check on fresh rows. The `self_managed`
                // flip below also pins `tenant_type_uuid = expected` as
                // defence-in-depth.
                if tenant_row.tenant_type_uuid != input.expected_tenant_type_uuid {
                    return Err(TxError::Domain(DomainError::Validation {
                        detail: format!(
                            "tenant {} type changed under TX (expected {}, observed {}); \
                             retry approve so the service re-runs the type compatibility check",
                            tenant_row.id,
                            input.expected_tenant_type_uuid,
                            tenant_row.tenant_type_uuid,
                        ),
                    }));
                }

                // 3. Re-load the parent tenant for the parent-side
                //    status precondition + the parent-side TOCTOU
                //    guard on `tenant_type_uuid`.
                //
                // No `deleted_at IS NULL` filter on the load: a peer
                // soft-delete of the parent between request and
                // approve leaves the row in place with `deleted_at`
                // set, and the failure is recoverable user-state
                // (operator un-deletes / caller retries after the
                // parent is reactivated), NOT a system fault. The
                // service-layer `approve` prechecks the parent for
                // `Active` before reaching this TX, so this branch
                // is defence-in-depth — but the disambiguation
                // matters: a hard-delete is structurally impossible
                // here (the FK cascade on `conversion_requests.parent_id`
                // would have removed this request row first), so a
                // truly missing row is the only `Internal` path; a
                // soft-deleted or suspended parent surfaces as
                // `Validation` to mirror the converting-tenant
                // status-flip arm at lines 634-652.
                let parent_row = tenants::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(Condition::all().add(tenants::Column::Id.eq(parent_id_from_req)))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| {
                        TxError::Domain(DomainError::Internal {
                            diagnostic: format!(
                                "conversion {}: parent tenant {parent_id_from_req} hard-deleted \
                                 between request and approve (FK cascade should have removed this \
                                 request row first)",
                                input.request_id
                            ),
                            cause: None,
                        })
                    })?;
                if parent_row.status != TenantStatus::Active.as_smallint() {
                    let observed = TenantStatus::from_smallint(parent_row.status)
                        .map_or_else(|| parent_row.status.to_string(), |s| s.as_str().to_owned());
                    return Err(TxError::Domain(DomainError::Validation {
                        detail: format!(
                            "parent tenant {} is not active (status={observed})",
                            parent_row.id,
                        ),
                    }));
                }
                // TOCTOU guard mirroring the converting-tenant arm
                // above. Type-compatibility was evaluated by the
                // service on the parent snapshot it loaded outside
                // this TX; a peer retype since then forces the
                // service to re-run the check on fresh rows.
                if parent_row.tenant_type_uuid != input.expected_parent_tenant_type_uuid {
                    return Err(TxError::Domain(DomainError::Validation {
                        detail: format!(
                            "parent tenant {} type changed under TX (expected {}, observed {}); \
                             retry approve so the service re-runs the type compatibility check",
                            parent_row.id,
                            input.expected_parent_tenant_type_uuid,
                            parent_row.tenant_type_uuid,
                        ),
                    }));
                }

                // 4. Flip `tenants.self_managed`.
                //
                // The UPDATE filter pins the tenant id, `status =
                // Active`, AND `tenant_type_uuid = expected` so a peer
                // that committed a suspend / soft-delete OR a retype
                // between the step-2 precheck and this point cannot
                // have its tenant silently flipped under us. The
                // explicit reload + checks above already cover both
                // races; the WHERE-clause predicate is defence-in-
                // depth against a future caller that bypasses the
                // reload. On `rows_affected == 0` we surface
                // `Validation` (status / type changed under our feet)
                // — the caller's apply rolls back, the pending row
                // remains for a retry once the operator reconciles
                // the tenant. `updated_at` is stamped from the service-
                // supplied `resolved_at` so the column write is
                // deterministic alongside the request transition.
                let new_self_managed = matches!(input.target_mode, TargetMode::SelfManaged);
                let flip_res = tenants::Entity::update_many()
                    .col_expr(tenants::Column::SelfManaged, Expr::value(new_self_managed))
                    .col_expr(tenants::Column::UpdatedAt, Expr::value(input.resolved_at))
                    .filter(
                        Condition::all()
                            .add(tenants::Column::Id.eq(input.target_tenant_id))
                            .add(tenants::Column::Status.eq(TenantStatus::Active.as_smallint()))
                            .add(
                                tenants::Column::TenantTypeUuid.eq(input.expected_tenant_type_uuid),
                            )
                            .add(tenants::Column::DeletedAt.is_null()),
                    )
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?;
                if flip_res.rows_affected == 0 {
                    return Err(TxError::Domain(DomainError::Validation {
                        detail: format!(
                            "tenant {} flip predicate did not match under TX (status or \
                             tenant_type_uuid changed between precheck and flip); retry approve",
                            input.target_tenant_id
                        ),
                    }));
                }

                // 5. Recompute closure-row barriers for every row
                //    whose strict (ancestor, descendant] path crosses
                //    the converted tenant. The strategy snapshots
                //    the relevant `(id, parent_id, self_managed)`
                //    triples in-Rust and walks each affected closure
                //    row's strict path against the post-flip values
                //    so the recompute is engine-agnostic. Any closure
                //    row whose path does NOT cross the converted
                //    tenant retains its existing barrier.
                //
                // No `deleted_at IS NULL` filter on the snapshot:
                // `tenant_closure` rows are retained for soft-deleted
                // descendants until the FK CASCADE fires on hard-
                // delete, so the barrier MUST be recomputed for those
                // rows too. Excluding soft-deleted tenants from
                // `parent_map` would cause `strict_path_crosses_impl`
                // to fail to walk through them, silently leaving
                // stale barrier values until retention cleanup — the
                // integrity checker would then flag the divergence.
                //
                // TODO(scale): full-table `tenants` load inside the
                // SERIALIZABLE retry loop. At ~10K+ tenants this is a
                // multi-MB heap allocation per retry attempt and
                // widens the SI conflict surface. Replace with a
                // closure-bounded query (load only tenants on the
                // strict path of the converted subtree) once a
                // closure-walk helper exists — the `tenant_closure`
                // table is the natural anchor (same machinery
                // [`ScopeFilter::in_tenant_subtree`](modkit_security::ScopeFilter::in_tenant_subtree)
                // compiles against). Today this is the only
                // engine-agnostic shape that keeps the barrier
                // recompute correct for soft-deleted descendants.
                //
                // NOTE(ordering): this snapshot MUST run after the
                // step-4 `tenants.self_managed` flip above —
                // `recompute_barriers_for_subtree` consumes
                // `self_managed_map` for the converted tenant and
                // expects the POST-flip value. A future refactor that
                // hoists this read above the flip silently feeds
                // stale data into the barrier rewrite (no test pins
                // the order today; the integrity checker would only
                // flag the divergence after the fact).
                let tenant_snapshots: Vec<(Uuid, Option<Uuid>, bool)> = tenants::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .all(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .into_iter()
                    .map(|t| (t.id, t.parent_id, t.self_managed))
                    .collect();
                let parent_map: HashMap<Uuid, Option<Uuid>> = tenant_snapshots
                    .iter()
                    .map(|(id, p, _)| (*id, *p))
                    .collect();
                let self_managed_map: HashMap<Uuid, bool> = tenant_snapshots
                    .iter()
                    .map(|(id, _, sm)| (*id, *sm))
                    .collect();

                // Affected rows: every closure row whose strict path
                // crosses the converted tenant. Cheap pre-filter:
                // either ancestor_id == target, or descendant_id ==
                // target, OR the row's strict path includes target.
                // We pull every row referencing the target as either
                // endpoint plus every row in the converted tenant's
                // descendant set (via closure self-join).
                //
                // Simpler portable strategy (works on SQLite + PG):
                // pull every closure row where `descendant_id` is in
                // the converted tenant's descendants set (descendants
                // INCLUSIVE of the target). For each such row,
                // recompute the barrier — the path crosses the
                // target iff `ancestor_id` is a strict ancestor of
                // (or equal to) the target's parent.
                let descendants_of_target: Vec<Uuid> = tenant_closure::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(
                        Condition::all()
                            .add(tenant_closure::Column::AncestorId.eq(input.target_tenant_id)),
                    )
                    .all(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .into_iter()
                    .map(|r| r.descendant_id)
                    .collect();

                if !descendants_of_target.is_empty() {
                    // Chunk the `IN (...)` over the descendant id list
                    // so a wide subtree cannot approach the Postgres
                    // 65 535-bind-parameter ceiling and turn a
                    // recoverable approve into a hard failure
                    // mid-transaction. The 4 096 chunk size leaves
                    // ample headroom for SeaORM-internal binds and
                    // keeps the per-query plan cost bounded; on a
                    // typical subtree the loop runs once.
                    //
                    // Long-term, the self-join on `tenant_closure`
                    // (`tc1 JOIN tc2 ON tc1.descendant_id =
                    // tc2.descendant_id WHERE tc2.ancestor_id =
                    // target`) is the right shape — chunking is the
                    // portable Phase-1 substitute until that lands
                    // (tracked alongside the recursive-CTE work).
                    const CLOSURE_DESCENDANTS_IN_CHUNK: usize = 4_096;
                    let mut candidate_rows: Vec<tenant_closure::Model> = Vec::new();
                    for chunk in descendants_of_target.chunks(CLOSURE_DESCENDANTS_IN_CHUNK) {
                        let chunk_rows = tenant_closure::Entity::find()
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .filter(Condition::all().add(
                                tenant_closure::Column::DescendantId.is_in(chunk.iter().copied()),
                            ))
                            .all(tx)
                            .await
                            .map_err(map_scope_to_tx)?;
                        candidate_rows.extend(chunk_rows);
                    }
                    for row in candidate_rows {
                        if row.ancestor_id == row.descendant_id {
                            continue; // self-row keeps barrier 0
                        }
                        if !strict_path_crosses_impl(
                            &parent_map,
                            row.ancestor_id,
                            row.descendant_id,
                            input.target_tenant_id,
                        ) {
                            continue;
                        }
                        let new_barrier = i16::from(strict_path_has_self_managed_impl(
                            &parent_map,
                            &self_managed_map,
                            row.ancestor_id,
                            row.descendant_id,
                        ));
                        if new_barrier == row.barrier {
                            continue;
                        }
                        tenant_closure::Entity::update_many()
                            .col_expr(tenant_closure::Column::Barrier, Expr::value(new_barrier))
                            .filter(
                                Condition::all()
                                    .add(tenant_closure::Column::AncestorId.eq(row.ancestor_id))
                                    .add(
                                        tenant_closure::Column::DescendantId.eq(row.descendant_id),
                                    ),
                            )
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .exec(tx)
                            .await
                            .map_err(map_scope_to_tx)?;
                    }
                }

                // 6. Stamp the request transition LAST so any earlier
                //    failure rolls back via the surrounding TX.
                //
                // The fence repeats `status = Pending AND deleted_at IS
                // NULL` even though step 1 already re-loaded the row in
                // the same TX: on PG, `with_serializable_retry` catches
                // a peer apply via `40001` SERIALIZABLE failure; on
                // SQLite there is no SERIALIZABLE level, so this fence
                // is the only signal that a peer apply landed first
                // and surfaces it as `AlreadyResolved` instead of a
                // silent zero-rows path. Mirrors the
                // `run_guarded_transition` rows-affected disambiguation.
                let stamp_res = conversion_requests::Entity::update_many()
                    .col_expr(
                        conversion_requests::Column::Status,
                        Expr::value(ConversionStatus::Approved.as_smallint()),
                    )
                    .col_expr(
                        conversion_requests::Column::ApprovedBy,
                        Expr::value(Some(input.approver_uuid)),
                    )
                    .col_expr(
                        conversion_requests::Column::ResolvedAt,
                        Expr::value(Some(input.resolved_at)),
                    )
                    .filter(
                        Condition::all()
                            .add(conversion_requests::Column::Id.eq(input.request_id))
                            .add(
                                conversion_requests::Column::Status
                                    .eq(ConversionStatus::Pending.as_smallint()),
                            )
                            .add(conversion_requests::Column::DeletedAt.is_null()),
                    )
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?;
                if stamp_res.rows_affected == 0 {
                    return Err(TxError::Domain(DomainError::AlreadyResolved));
                }

                let fresh = conversion_requests::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(id_eq_alive(input.request_id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| {
                        TxError::Domain(DomainError::Internal {
                            diagnostic: format!(
                                "conversion {} disappeared after successful apply",
                                input.request_id
                            ),
                            cause: None,
                        })
                    })?;
                entity_to_conversion(fresh).map_err(TxError::Domain)
            })
        })
    })
    .await
}
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-mixed-mode-tree-consistency:p1:inst-dod-mixed-mode-tree-consistency-impl
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-barrier-rematerialization-consistency:p1:inst-dod-barrier-rematerialization
// @cpt-end:cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply:p1:inst-dod-dual-consent-apply-impl
// @cpt-end:cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply:p1:inst-algo-dual-consent-apply-impl

/// Walk the strict `(ancestor, descendant]` path in the parent map and
/// return `true` iff the `target` tenant appears on it.
///
/// Cycle-safe via `parent_map.len()` as the hop cap. A non-cyclic
/// walk through a parent map of size N visits each node at most
/// once: `while let Some(_)` rejects the `None` returned by
/// `parent_map.get(root)` BEFORE entering the body, so the body
/// runs at most N times for a legitimate path. The guard
/// `hops > cap` is checked at the start of each iteration BEFORE
/// the increment; it can only fire after the body has already
/// executed at least N+1 times — a state only reachable when a
/// cycle has caused the loop to re-enter a previously-visited
/// node, at which point we have already iterated past the
/// snapshot size. The one-hop slack between "snapshot size" and
/// "guard fires" is intentional — a non-strict `hops == cap`
/// form would short-circuit a path of exactly length N before
/// its final node could match. The previous fixed cap of `1024`
/// was tighter than `AccountManagementConfig::MAX_DEPTH_THRESHOLD`
/// (`1_000_000`) and would silently truncate legitimate deep
/// paths in large-hierarchy deployments — leaving stale barriers
/// after a successful approve. Sizing the cap to the snapshot
/// makes the walk correct for any depth representable in the map,
/// while preserving the cycle-safety guarantee. Emits a `warn!`
/// on `am.domain` when the cap fires so the data-integrity event
/// is observable instead of being silently swallowed.
fn strict_path_crosses_impl(
    parent_map: &HashMap<Uuid, Option<Uuid>>,
    ancestor: Uuid,
    descendant: Uuid,
    target: Uuid,
) -> bool {
    let cap = parent_map.len();
    let mut current = Some(descendant);
    let mut hops = 0_usize;
    while let Some(node) = current {
        if hops > cap {
            tracing::warn!(
                target: "am.domain",
                ancestor = %ancestor,
                descendant = %descendant,
                cap,
                "strict_path_crosses_impl: hop cap exceeded (cycle or duplicate edge in parent map); \
                 returning false to stay cycle-safe"
            );
            return false;
        }
        if node == ancestor {
            return false;
        }
        if node == target {
            return true;
        }
        current = parent_map.get(&node).copied().flatten();
        hops += 1;
    }
    false
}

/// Walk the strict `(ancestor, descendant]` path and return `true` iff
/// any tenant on it has `self_managed = true` in the snapshot.
///
/// Hop cap follows the same `parent_map.len()` rule as
/// [`strict_path_crosses_impl`]; see that function's docs for the
/// rationale and observability contract on cycle detection.
fn strict_path_has_self_managed_impl(
    parent_map: &HashMap<Uuid, Option<Uuid>>,
    self_managed_map: &HashMap<Uuid, bool>,
    ancestor: Uuid,
    descendant: Uuid,
) -> bool {
    let cap = parent_map.len();
    let mut current = Some(descendant);
    let mut hops = 0_usize;
    while let Some(node) = current {
        if hops > cap {
            tracing::warn!(
                target: "am.domain",
                ancestor = %ancestor,
                descendant = %descendant,
                cap,
                "strict_path_has_self_managed_impl: hop cap exceeded (cycle or duplicate edge \
                 in parent map); returning false to stay cycle-safe"
            );
            return false;
        }
        if node == ancestor {
            return false;
        }
        if self_managed_map.get(&node).copied().unwrap_or(false) {
            return true;
        }
        current = parent_map.get(&node).copied().flatten();
        hops += 1;
    }
    false
}

async fn list_own_for_tenant(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    status_filter: Option<ConversionStatus>,
    pagination: ConversionPagination,
) -> Result<Vec<ConversionRequest>, DomainError> {
    list_by(
        repo,
        scope,
        Condition::all().add(conversion_requests::Column::TenantId.eq(tenant_id)),
        status_filter,
        pagination,
    )
    .await
}

async fn list_inbound_for_parent(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    parent_id: Uuid,
    status_filter: Option<ConversionStatus>,
    pagination: ConversionPagination,
) -> Result<Vec<ConversionRequest>, DomainError> {
    list_by(
        repo,
        scope,
        Condition::all().add(conversion_requests::Column::ParentId.eq(parent_id)),
        status_filter,
        pagination,
    )
    .await
}

async fn count_own_for_tenant(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    status_filter: Option<ConversionStatus>,
) -> Result<u64, DomainError> {
    count_by(
        repo,
        scope,
        Condition::all().add(conversion_requests::Column::TenantId.eq(tenant_id)),
        status_filter,
    )
    .await
}

async fn count_inbound_for_parent(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    parent_id: Uuid,
    status_filter: Option<ConversionStatus>,
) -> Result<u64, DomainError> {
    count_by(
        repo,
        scope,
        Condition::all().add(conversion_requests::Column::ParentId.eq(parent_id)),
        status_filter,
    )
    .await
}

/// Shared count helper that applies the same predicate `list_by` uses
/// (caller-supplied base + optional `status_filter` + soft-delete
/// exclusion) and returns the row count without pagination. Used by
/// the service layer to populate `OffsetPage.total` correctly when
/// `top < total` or `skip > 0`.
async fn count_by(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    base: Condition,
    status_filter: Option<ConversionStatus>,
) -> Result<u64, DomainError> {
    let conn = repo.db.conn()?;
    let mut filter = base.add(conversion_requests::Column::DeletedAt.is_null());
    if let Some(status) = status_filter {
        filter = filter.add(conversion_requests::Column::Status.eq(status.as_smallint()));
    }
    conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter)
        .count(&conn)
        .await
        .map_err(map_scope_err)
}

/// Shared listing body. Applies the caller-supplied base filter, the
/// optional `status_filter`, the always-on `deleted_at IS NULL`
/// predicate, and the documented stable ordering
/// `(requested_at DESC, id ASC)` before paginating with `top` /
/// `skip`.
async fn list_by(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    base: Condition,
    status_filter: Option<ConversionStatus>,
    pagination: ConversionPagination,
) -> Result<Vec<ConversionRequest>, DomainError> {
    let conn = repo.db.conn()?;
    let mut filter = base.add(conversion_requests::Column::DeletedAt.is_null());
    if let Some(status) = status_filter {
        filter = filter.add(conversion_requests::Column::Status.eq(status.as_smallint()));
    }
    let rows = conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter)
        .order_by(conversion_requests::Column::RequestedAt, Order::Desc)
        .order_by(conversion_requests::Column::Id, Order::Asc)
        .limit(u64::from(pagination.top))
        .offset(u64::from(pagination.skip))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(entity_to_conversion(r)?);
    }
    Ok(out)
}

async fn query_expired(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    cutoff: OffsetDateTime,
    batch_size: u32,
) -> Result<Vec<ConversionRequest>, DomainError> {
    let conn = repo.db.conn()?;
    let rows = conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(
                    conversion_requests::Column::Status.eq(ConversionStatus::Pending.as_smallint()),
                )
                // Inclusive `<=` mirrors the trait contract documented
                // on `ConversionRepo::query_expired` (`expires_at <=
                // cutoff`) and the fake's predicate in
                // `test_support/repo.rs` — a request whose `expires_at`
                // equals the cutoff instant has reached the deadline
                // and SHOULD be expired this tick. The asymmetry vs.
                // the retention sweep below (which uses strict `<` on
                // `resolved_at`) is intentional: retention spares rows
                // resolved at exactly the cutoff for one extra tick so
                // an audit reader observing the listing at the cutoff
                // boundary sees them, while expiry runs at the
                // deadline because the FEATURE doc binds the lifecycle
                // to "now() >= expires_at".
                .add(conversion_requests::Column::ExpiresAt.lte(cutoff))
                .add(conversion_requests::Column::DeletedAt.is_null()),
        )
        .order_by(conversion_requests::Column::ExpiresAt, Order::Asc)
        .order_by(conversion_requests::Column::Id, Order::Asc)
        .limit(u64::from(batch_size))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(entity_to_conversion(r)?);
    }
    Ok(out)
}

async fn soft_delete_resolved_older_than(
    repo: &ConversionRepoImpl,
    scope: &AccessScope,
    cutoff: OffsetDateTime,
    now: OffsetDateTime,
    batch_size: u32,
) -> Result<u64, DomainError> {
    let conn = repo.db.conn()?;
    // Bound the per-call write set with a SELECT-by-cutoff so the
    // driving cap (`batch_size`) is honoured even on engines whose
    // `UPDATE … LIMIT` support is unreliable across SeaORM dialect
    // back-ends. The two-statement pattern matches the retention-
    // pipeline scan/claim shape used in `repo_impl::retention`.
    let candidate_ids: Vec<Uuid> = conversion_requests::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                // Resolved rows only: the four non-`Pending` variants.
                // Expressed as `status != Pending` to keep the impl
                // resilient if a future status variant is added.
                .add(
                    conversion_requests::Column::Status.ne(ConversionStatus::Pending.as_smallint()),
                )
                .add(conversion_requests::Column::ResolvedAt.is_not_null())
                // Strict `<` matches the trait contract documented in
                // `ConversionRepo::soft_delete_resolved_older_than`
                // (`resolved_at < cutoff`) and the fake's predicate in
                // `test_support/repo.rs`; using `.lte` here would
                // silently soft-delete rows resolved exactly at the
                // cutoff instant in production while the fake (and
                // every test pinned against it) retained them, drifting
                // boundary-case retention assertions between paths.
                .add(conversion_requests::Column::ResolvedAt.lt(cutoff))
                .add(conversion_requests::Column::DeletedAt.is_null()),
        )
        .order_by(conversion_requests::Column::ResolvedAt, Order::Asc)
        .order_by(conversion_requests::Column::Id, Order::Asc)
        .limit(u64::from(batch_size))
        .all(&conn)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .map(|r| r.id)
        .collect();
    if candidate_ids.is_empty() {
        return Ok(0);
    }

    let res = conversion_requests::Entity::update_many()
        .col_expr(
            conversion_requests::Column::DeletedAt,
            Expr::value(Some(now)),
        )
        .filter(
            Condition::all()
                .add(conversion_requests::Column::Id.is_in(candidate_ids))
                // Re-assert eligibility on the UPDATE so a peer write
                // that resurrected a row (e.g. by clearing
                // `deleted_at` — currently impossible from the trait
                // surface, defensive in case of future ops tooling)
                // cannot be silently overwritten.
                .add(
                    conversion_requests::Column::Status.ne(ConversionStatus::Pending.as_smallint()),
                )
                .add(conversion_requests::Column::DeletedAt.is_null()),
        )
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(res.rows_affected)
}

// ---------------------------------------------------------------------------
// Trait dispatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl ConversionRepo for ConversionRepoImpl {
    async fn insert_pending(
        &self,
        scope: &AccessScope,
        new: &NewConversionRequest,
    ) -> Result<ConversionRequest, DomainError> {
        insert_pending(self, scope, new).await
    }

    async fn find_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError> {
        find_by_id(self, scope, id).await
    }

    async fn find_pending_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Option<ConversionRequest>, DomainError> {
        find_pending_for_tenant(self, scope, tenant_id).await
    }

    async fn __transition_pending_to_approved_test_only(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        approved_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        __transition_pending_to_approved_test_only(
            self,
            scope,
            request_id,
            approved_by,
            resolved_at,
        )
        .await
    }

    async fn apply_conversion_approval(
        &self,
        scope: &AccessScope,
        input: ApplyConversionApprovalInput,
    ) -> Result<ConversionRequest, DomainError> {
        apply_conversion_approval(self, scope, input).await
    }

    async fn transition_pending_to_cancelled(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        cancelled_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        transition_pending_to_cancelled(self, scope, request_id, cancelled_by, resolved_at).await
    }

    async fn transition_pending_to_rejected(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        rejected_by: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        transition_pending_to_rejected(self, scope, request_id, rejected_by, resolved_at).await
    }

    async fn transition_pending_to_expired(
        &self,
        scope: &AccessScope,
        request_id: Uuid,
        resolved_at: OffsetDateTime,
    ) -> Result<ConversionRequest, DomainError> {
        transition_pending_to_expired(self, scope, request_id, resolved_at).await
    }

    async fn list_own_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        list_own_for_tenant(self, scope, tenant_id, status_filter, pagination).await
    }

    async fn list_inbound_for_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
        pagination: ConversionPagination,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        list_inbound_for_parent(self, scope, parent_id, status_filter, pagination).await
    }

    async fn count_own_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError> {
        count_own_for_tenant(self, scope, tenant_id, status_filter).await
    }

    async fn count_inbound_for_parent(
        &self,
        scope: &AccessScope,
        parent_id: Uuid,
        status_filter: Option<ConversionStatus>,
    ) -> Result<u64, DomainError> {
        count_inbound_for_parent(self, scope, parent_id, status_filter).await
    }

    async fn query_expired(
        &self,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        batch_size: u32,
    ) -> Result<Vec<ConversionRequest>, DomainError> {
        query_expired(self, scope, cutoff, batch_size).await
    }

    async fn soft_delete_resolved_older_than(
        &self,
        scope: &AccessScope,
        cutoff: OffsetDateTime,
        now: OffsetDateTime,
        batch_size: u32,
    ) -> Result<u64, DomainError> {
        soft_delete_resolved_older_than(self, scope, cutoff, now, batch_size).await
    }
}

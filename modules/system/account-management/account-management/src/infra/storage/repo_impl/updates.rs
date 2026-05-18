//! Mutating writes outside the create/destroy lifecycle:
//! `update_tenant_mutable`, `set_status`,
//! `load_ancestor_chain_through_parent`, `schedule_deletion`. All
//! transactional writes go through
//! [`super::helpers::with_serializable_retry`] under `SERIALIZABLE`
//! isolation per AC#15.

use std::collections::HashSet;
use std::time::Duration;

use modkit_db::secure::{DbTx, SecureEntityExt, SecureUpdateExt};
use modkit_security::AccessScope;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryFilter};
use time::OffsetDateTime;
use uuid::Uuid;

use account_management_sdk::UpdateTenantRequest;

use crate::domain::error::DomainError;
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::infra::storage::entity::{tenant_closure, tenants};

use super::TenantRepoImpl;
use super::helpers::{
    TxError, entity_to_model, id_eq, map_scope_err, map_scope_to_tx, with_serializable_retry,
};

pub(super) async fn update_tenant_mutable(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    patch: &UpdateTenantRequest,
) -> Result<TenantModel, DomainError> {
    let patch_owned = patch.clone();
    let scope = scope.clone();
    with_serializable_retry(&repo.db, move || {
        let patch_owned = patch_owned.clone();
        let scope = scope.clone();
        Box::new(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                // SERIALIZABLE anti-dependency anchor: the SELECT
                // forces this transaction to see the row in its
                // pre-update state, so a concurrent PATCH on the
                // same row triggers a `40001` serialization
                // failure instead of a lost update. The row value
                // is also re-checked for status eligibility on
                // every retry, so a concurrent soft-delete
                // committing between the original attempt and the
                // retry cannot resurrect a `Deleted` row through a
                // mutable patch.
                let row = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(tenant_id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::NotFound {
                        detail: format!("tenant {tenant_id} not found"),
                        resource: tenant_id.to_string(),
                    })?;
                if row.status == TenantStatus::Deleted.as_smallint() {
                    return Err(DomainError::Conflict {
                        detail: format!("tenant {tenant_id} is deleted and not mutable"),
                    }
                    .into());
                }
                if row.status == TenantStatus::Provisioning.as_smallint() {
                    return Err(DomainError::Conflict {
                        detail: format!(
                            "tenant {tenant_id} is provisioning and not mutable through PATCH"
                        ),
                    }
                    .into());
                }

                // Idempotent no-op detection (HTTP PATCH semantics,
                // option A — true idempotency). The patch shape is
                // name-only (status transitions go through
                // `set_status`); if the patched name matches the
                // current row, skip the UPDATE so `updated_at` stays
                // unchanged and the caller observes identical state
                // on retry.
                let name_to_write = patch_owned
                    .name
                    .as_ref()
                    .filter(|n| n.as_str() != row.name.as_str());
                if name_to_write.is_none() {
                    return entity_to_model(row).map_err(TxError::Domain);
                }

                let now = OffsetDateTime::now_utc();
                let mut upd = tenants::Entity::update_many()
                    .col_expr(tenants::Column::UpdatedAt, Expr::value(now));
                if let Some(new_name) = name_to_write {
                    upd = upd.col_expr(tenants::Column::Name, Expr::value(new_name.clone()));
                }
                upd.filter(id_eq(tenant_id))
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?;

                let fresh = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(tenant_id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::Internal {
                        diagnostic: format!("tenant {tenant_id} disappeared after update"),
                        cause: None,
                    })?;
                entity_to_model(fresh).map_err(TxError::Domain)
            })
        })
    })
    .await
}

/// Flip `tenants.status` between `Active` and `Suspended` (with a
/// matching rewrite of `tenant_closure.descendant_status`) under
/// SERIALIZABLE isolation. Same-to-same is an idempotent no-op and
/// returns the current row without emitting an UPDATE. `Deleted` /
/// `Provisioning` either as the current state or as `new_status`
/// surface as `Conflict` — only the dedicated soft-delete /
/// provisioning flows are allowed to move the row in or out of those
/// states.
// Note on `now`: the wall-clock is captured once by the caller and
// reused across every SERIALIZABLE retry. On a highly contended row
// the committed `updated_at` may therefore lag the actual commit
// time by however long the retry loop took. This mirrors the
// established pattern in `schedule_deletion` and is acceptable
// because (1) `updated_at` is informational, not load-bearing for any
// invariant, and (2) re-stamping `now` per retry would defeat the
// `was_no_op` heuristic in the service layer (caller compares
// pre/post `updated_at` to suppress audit on same-to-same calls).
pub(super) async fn set_status(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    new_status: TenantStatus,
    now: OffsetDateTime,
) -> Result<TenantModel, DomainError> {
    // Reject the inadmissible targets eagerly so the SERIALIZABLE TX
    // is not entered on a request that cannot succeed. The repo
    // contract reserves `Deleted` for `schedule_deletion` and
    // `Provisioning` for the create-tenant saga.
    match new_status {
        TenantStatus::Active | TenantStatus::Suspended => {}
        TenantStatus::Deleted => {
            return Err(DomainError::Conflict {
                detail: format!(
                    "tenant {tenant_id}: set_status cannot transition to deleted; \
                     use the soft-delete flow (`schedule_deletion`)"
                ),
            });
        }
        TenantStatus::Provisioning => {
            return Err(DomainError::Conflict {
                detail: format!("tenant {tenant_id}: set_status cannot transition to provisioning"),
            });
        }
    }
    let scope = scope.clone();
    with_serializable_retry(&repo.db, move || {
        let scope = scope.clone();
        Box::new(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                let row = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(tenant_id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::NotFound {
                        detail: format!("tenant {tenant_id} not found"),
                        resource: tenant_id.to_string(),
                    })?;
                if row.status == TenantStatus::Deleted.as_smallint() {
                    return Err(DomainError::Conflict {
                        detail: format!(
                            "tenant {tenant_id} is deleted; status is terminal during retention"
                        ),
                    }
                    .into());
                }
                if row.status == TenantStatus::Provisioning.as_smallint() {
                    return Err(DomainError::Conflict {
                        detail: format!(
                            "tenant {tenant_id} is provisioning; lifecycle transitions are not \
                             permitted until activation completes"
                        ),
                    }
                    .into());
                }

                // Same-to-same: idempotent no-op. Skip the UPDATE so
                // `updated_at` is unchanged and the closure rewrite
                // does not run.
                if row.status == new_status.as_smallint() {
                    return entity_to_model(row).map_err(TxError::Domain);
                }

                tenants::Entity::update_many()
                    .col_expr(
                        tenants::Column::Status,
                        Expr::value(new_status.as_smallint()),
                    )
                    .col_expr(tenants::Column::UpdatedAt, Expr::value(now))
                    .filter(id_eq(tenant_id))
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?;

                // Rewrite tenant_closure.descendant_status atomically.
                // `tenant_closure` is declared
                // `no_tenant/no_resource/no_owner/no_type` so
                // `scope_with(scope)` would compile to either a no-op
                // (`allow_all`) or `WHERE false` (any narrowed scope).
                // Authorization was enforced upstream at the PDP gate;
                // pass `allow_all` here so the closure write is
                // unaffected by the caller's scope shape.
                // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance:p1:inst-algo-closmnt-repo-status-update
                let closure_rows_affected = tenant_closure::Entity::update_many()
                    .col_expr(
                        tenant_closure::Column::DescendantStatus,
                        Expr::value(new_status.as_smallint()),
                    )
                    .filter(
                        Condition::all().add(tenant_closure::Column::DescendantId.eq(tenant_id)),
                    )
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .rows_affected;
                // Closure self-row invariant: every SDK-visible tenant
                // has a `(id, id)` row, so this UPDATE must touch at
                // least the self-row. Zero here means the self-row is
                // missing — a concrete invariant breach the integrity
                // classifier would only flag retroactively. Fail the
                // tx so SERIALIZABLE rolls back the `tenants.status`
                // flip rather than leaving `tenant_closure` stale.
                if closure_rows_affected == 0 {
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "set_status: closure descendant_status rewrite for tenant \
                             {tenant_id} affected 0 rows; self-row missing"
                        ),
                        cause: None,
                    }
                    .into());
                }
                // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance:p1:inst-algo-closmnt-repo-status-update

                let fresh = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(tenant_id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::Internal {
                        diagnostic: format!("tenant {tenant_id} disappeared after set_status"),
                        cause: None,
                    })?;
                entity_to_model(fresh).map_err(TxError::Domain)
            })
        })
    })
    .await
}

pub(super) async fn load_ancestor_chain_through_parent(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    parent_id: Uuid,
) -> Result<Vec<TenantModel>, DomainError> {
    let conn = repo.db.conn()?;

    // Resolve the ancestor chain (parent + its strict ancestors) via
    // parent's closure rows: every `descendant_id = parent_id` row in
    // `tenant_closure` names a tenant on the chain `[parent, parent's
    // parent, …, root]` — including parent itself via its own
    // self-row. One RT into closure → one RT into `tenants` for the
    // chain, regardless of depth.
    //
    // `tenant_closure` carries no per-row tenant ownership column
    // (`no_tenant`, `no_resource`) — closure rows are cross-tenant
    // by definition — so the scoped read is the subsequent
    // `tenants` query.
    let closure_rows = tenant_closure::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(tenant_closure::Column::DescendantId.eq(parent_id)))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    if closure_rows.is_empty() {
        // Defensive fallback: parent has no closure rows yet (e.g.
        // diagnostic call against an in-flight provisioning parent).
        // Walk via `parent_id` like the legacy implementation; this
        // path is rare and bounded by the depth invariant.
        //
        // The walk is hard-capped at `MAX_ANCESTOR_WALK_HOPS` so a
        // corrupt `parent_id` cycle (or a depth value that disagrees
        // with the actual chain length) can't loop indefinitely
        // issuing one DB round-trip per hop. The cap is well above
        // any realistic depth_threshold; overrun returns
        // `DomainError::Internal` so ops sees the corruption.
        const MAX_ANCESTOR_WALK_HOPS: usize = 64;
        // Emit a WARN whenever we land here so a closure-table gap
        // developing in production (the only non-bootstrap reason
        // this branch fires) is surfaced rather than silently
        // absorbing the per-hop round-trips.
        tracing::warn!(
            target: "am.tenant_repo",
            parent_id = %parent_id,
            "load_ancestor_chain_through_parent: closure rows empty; falling back to parent_id walk (one query per hop)"
        );
        let mut chain = Vec::new();
        let mut cursor_id = Some(parent_id);
        let mut hops = 0usize;
        while let Some(pid) = cursor_id {
            if hops >= MAX_ANCESTOR_WALK_HOPS {
                return Err(DomainError::Internal {
                    diagnostic: format!(
                        "ancestor walk exceeded {MAX_ANCESTOR_WALK_HOPS} hops from parent {parent_id}; possible parent_id cycle"
                    ),
                    cause: None,
                });
            }
            // Structural lineage read: the ancestor chain may extend
            // beyond the caller's scope (a tenant admin authorized to
            // their own subtree still needs the chain up to the root
            // for closure-row construction). Authorization for the
            // operation as a whole is enforced upstream by the PDP at
            // the service layer; this read is the structural truth
            // that gate consults, hence `allow_all`.
            let parent = tenants::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .filter(id_eq(pid))
                .one(&conn)
                .await
                .map_err(map_scope_err)?
                .ok_or_else(|| DomainError::NotFound {
                    detail: format!("ancestor {pid} missing while walking chain"),
                    resource: pid.to_string(),
                })?;
            // Suppress the otherwise-unused parameter warning; the
            // scope is part of the trait signature for consistency
            // with the caller-scoped reads, even though structural
            // lineage queries bypass it.
            let _ = scope;
            cursor_id = parent.parent_id;
            chain.push(entity_to_model(parent)?);
            hops += 1;
        }
        return Ok(chain);
    }

    let ancestor_ids: Vec<Uuid> = closure_rows.iter().map(|r| r.ancestor_id).collect();
    // Structural lineage read — see fallback walk above for the full
    // rationale. `allow_all` so a caller authorized on a non-root
    // parent (with their `AccessScope` narrowed to that subtree) can
    // still load the ancestors above the parent for closure-row
    // construction.
    let rows = tenants::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(tenants::Column::Id.is_in(ancestor_ids.clone())))
        .order_by(tenants::Column::Depth, Order::Desc)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    if rows.len() != ancestor_ids.len() {
        let found: HashSet<Uuid> = rows.iter().map(|r| r.id).collect();
        let mut missing_ids: Vec<Uuid> = ancestor_ids
            .iter()
            .copied()
            .filter(|id| !found.contains(id))
            .collect();
        missing_ids.sort_unstable();
        return Err(DomainError::NotFound {
            detail: format!("ancestor ids missing: {missing_ids:?}"),
            resource: format!("{missing_ids:?}"),
        });
    }

    let mut chain = Vec::with_capacity(rows.len());
    for r in rows {
        chain.push(entity_to_model(r)?);
    }
    // Adjacency check: the SQL ORDER BY `Depth DESC` produces the
    // chain leaf-first (the requested parent first, then its parent,
    // up to the root). For the chain to be a *contiguous* parent walk
    // each adjacent pair MUST satisfy `chain[i].parent_id ==
    // Some(chain[i+1].id)`, and the last entry (the root) MUST carry
    // `parent_id = None`. A count-only check (above) cannot detect a
    // closure-table gap that happens to load the right number of
    // unrelated rows; without this guard the caller would build
    // closure rows on a broken lineage and the integrity-classifier
    // would only flag it retroactively. Fail closed with `NotFound`.
    for i in 0..chain.len().saturating_sub(1) {
        let child = &chain[i];
        let expected_parent = &chain[i + 1];
        if child.parent_id != Some(expected_parent.id) {
            return Err(DomainError::NotFound {
                detail: format!(
                    "ancestor chain non-contiguous: tenant {child_id} has \
                     parent_id={child_parent:?} but the next chain entry is \
                     {expected_parent_id} (closure table reports an ancestor pair \
                     that is not actually a parent–child edge)",
                    child_id = child.id,
                    child_parent = child.parent_id,
                    expected_parent_id = expected_parent.id,
                ),
                resource: child.id.to_string(),
            });
        }
    }
    if let Some(last) = chain.last()
        && last.parent_id.is_some()
    {
        return Err(DomainError::NotFound {
            detail: format!(
                "ancestor chain top entry {last_id} carries parent_id={last_parent:?} \
                 but the chain MUST terminate at the root (parent_id = None); \
                 closure table reports a truncated ancestor walk",
                last_id = last.id,
                last_parent = last.parent_id,
            ),
            resource: last.id.to_string(),
        });
    }
    Ok(chain)
}

pub(super) async fn schedule_deletion(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    now: OffsetDateTime,
    retention: Option<Duration>,
) -> Result<TenantModel, DomainError> {
    // Fail-fast on overflow. Silently clamping a misconfigured
    // duration of e.g. `Duration::MAX` to ~292 billion years would
    // mask the misconfig and produce rows that never become
    // retention-due. Returning `Internal` surfaces the bug to ops
    // immediately.
    let retention_secs: Option<i64> = match retention {
        None => None,
        Some(d) => Some(
            i64::try_from(d.as_secs()).map_err(|_| DomainError::Internal {
                diagnostic: format!(
                    "retention duration {} secs overflows i64; misconfiguration",
                    d.as_secs()
                ),
                cause: None,
            })?,
        ),
    };
    let scope = scope.clone();
    with_serializable_retry(&repo.db, move || {
        let scope = scope.clone();
        Box::new(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                let existing = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::NotFound {
                        detail: format!("tenant {id} not found"),
                        resource: id.to_string(),
                    })?;
                // Idempotent short-circuit: already a tombstone. Return
                // the existing row WITHOUT re-stamping `deleted_at`,
                // re-running the closure rewrite, or pushing back the
                // retention deadline (a malicious retry could otherwise
                // indefinitely delay the hard-delete sweep). Symmetric
                // with the same-to-same no-op in `set_status` —
                // idempotency lives inside the SERIALIZABLE TX so two
                // racing DELETEs on the same Active row both observe
                // the post-flip state on the loser's retry and return
                // the tombstone instead of a 409. The service-layer
                // short-circuit in `delete_tenant` is a pure
                // optimisation to skip the RG ownership probe on the
                // un-contended retry path; this guard is the
                // authoritative correctness boundary.
                if existing.status == TenantStatus::Deleted.as_smallint() {
                    return entity_to_model(existing).map_err(TxError::Domain);
                }
                // `Provisioning` rows have no closure entries by
                // construction; flipping them straight to `Deleted`
                // would create an SDK-visible deleted tenant with no
                // self-row / ancestor rows. Provisioning rows are
                // cleaned up by the provisioning reaper, not by
                // soft-delete.
                if existing.status == TenantStatus::Provisioning.as_smallint() {
                    return Err(DomainError::Conflict {
                        detail: format!(
                            "tenant {id} is in provisioning state; \
                             use the provisioning reaper to compensate, not soft-delete"
                        ),
                    }
                    .into());
                }

                // Re-check inside the SERIALIZABLE write transaction.
                // The service performs the same guard before entering
                // the repo, but that pre-check can race a concurrent
                // create-child insert; this predicate read creates the
                // necessary serialization edge against that insert.
                let child_count = tenants::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(
                        Condition::all()
                            .add(tenants::Column::ParentId.eq(id))
                            .add(tenants::Column::Status.ne(TenantStatus::Deleted.as_smallint())),
                    )
                    .count(tx)
                    .await
                    .map_err(map_scope_to_tx)?;
                if child_count > 0 {
                    return Err(DomainError::TenantHasChildren.into());
                }

                let mut upd = tenants::Entity::update_many()
                    .col_expr(
                        tenants::Column::Status,
                        Expr::value(TenantStatus::Deleted.as_smallint()),
                    )
                    .col_expr(tenants::Column::UpdatedAt, Expr::value(now))
                    // `deleted_at` is both the public-contract
                    // tombstone exposed through the `Tenant` schema
                    // (`account-management-v1.yaml:591`) AND the
                    // retention-timer start consumed by the
                    // `idx_tenants_retention_scan` partial index
                    // declared by `m0001_initial_schema`. The
                    // hard-delete sweep becomes eligible at
                    // `deleted_at + retention_window` — see
                    // `scan_retention_due`.
                    .col_expr(tenants::Column::DeletedAt, Expr::value(now));
                if let Some(secs) = retention_secs {
                    upd = upd.col_expr(tenants::Column::RetentionWindowSecs, Expr::value(secs));
                }
                upd.filter(id_eq(id))
                    .secure()
                    .scope_with(&scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?;

                // Rewrite descendant_status on every closure row that
                // points at this tenant (same invariant as update).
                // `allow_all` for the same reason as
                // `update_tenant_mutable`: closure is
                // `no_tenant/no_resource/no_owner/no_type`, so a
                // narrowed scope would collapse to `WHERE false`
                // here because `tenant_closure` is property-less for
                // the [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
                // predicate. The `tenants` UPDATE above forwards the
                // caller's `scope` verbatim — that path is safe only
                // because the trait contract requires `allow_all`
                // (see `repo.rs` trait doc). Authorization is
                // enforced upstream at the PDP gate in the service
                // layer.
                // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance:p1:inst-algo-closmnt-repo-soft-delete-status
                let closure_rows_affected = tenant_closure::Entity::update_many()
                    .col_expr(
                        tenant_closure::Column::DescendantStatus,
                        Expr::value(TenantStatus::Deleted.as_smallint()),
                    )
                    .filter(Condition::all().add(tenant_closure::Column::DescendantId.eq(id)))
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .rows_affected;
                // Same closure self-row invariant as `update_tenant_mutable`:
                // an SDK-visible tenant must have a `(id, id)` row, so the
                // descendant-status rewrite must touch at least one row.
                // Zero means the self-row is missing — fail the tx so
                // SERIALIZABLE rolls back the `tenants.status -> Deleted`
                // flip rather than leaving `tenant_closure` stale.
                if closure_rows_affected == 0 {
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "schedule_deletion: closure descendant_status rewrite for tenant \
                             {id} affected 0 rows; self-row missing"
                        ),
                        cause: None,
                    }
                    .into());
                }
                // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance:p1:inst-algo-closmnt-repo-soft-delete-status

                let fresh = tenants::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(id_eq(id))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?
                    .ok_or_else(|| DomainError::Internal {
                        diagnostic: format!("tenant {id} disappeared after schedule_deletion"),
                        cause: None,
                    })?;
                entity_to_model(fresh).map_err(TxError::Domain)
            })
        })
    })
    .await
}

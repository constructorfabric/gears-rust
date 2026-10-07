//! @cpt-dod:cpt-cf-bss-products-dod-usage-type-resolves:p1
//! @cpt-dod:cpt-cf-bss-products-dod-terminal-audit-and-event:p1
//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-pin:p1
//! Shared scoped transaction plumbing for approval and reference operations.
use super::{ApiState, TxError, authz_error_to_canonical, contention_db_err, tx_to_canonical};
use crate::{
    authz::{access_scope, actions, labels, resource_types},
    domain::{
        derived, error::DomainError, recognized::UsageRefAnswer, validation::ValidationReport,
    },
    infra::{broker, events, storage::repo},
};
use authz_resolver_sdk::{PolicyEnforcer, pep::ResourceType};
use bss_approval::{Store, Unit};
use bss_products_sdk::models::{LifecycleNext, Sku, SkuContent};
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::{
    DbTx,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// The PDP's scope for `action` on `resource` (`resource_types::SKU` or `APPROVAL_UNIT`): the call
/// site names the resource it authorizes, not a `bool` (RS-54). A write anchors to the subject's
/// tenant; the 403 names the resource asked (RS-25).
pub async fn scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource: &ResourceType,
    action: &str,
) -> Result<AccessScope, CanonicalError> {
    access_scope(
        enforcer,
        ctx,
        resource,
        action,
        (action != actions::READ).then(|| ctx.subject_tenant_id()),
    )
    .await
    .map_err(|e| {
        authz_error_to_canonical(e, |reason| {
            crate::infra::error_mapping::permission_denied(resource.name(), reason)
        })
    })
}

/// The approve or submit grant for the flags on a unit read (P-D-255). A denial is an empty
/// scope, so a reader who cannot vote still gets the page. An unreachable PDP is 503.
pub async fn grant_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<AccessScope, CanonicalError> {
    match crate::authz::access_scope(
        enforcer,
        ctx,
        &resource_types::APPROVAL_UNIT,
        action,
        Some(ctx.subject_tenant_id()),
    )
    .await
    {
        Ok(scope) => Ok(scope),
        Err(crate::authz::AuthzError::Denied(_)) => Ok(AccessScope::deny_all()),
        Err(crate::authz::AuthzError::Unavailable(detail)) => {
            tracing::error!(detail, "bss-products: authorization service unavailable");
            Err(CanonicalError::service_unavailable().create())
        }
    }
}

/// Whether `scope` admits this unit. A filter that cannot be decided in memory does not.
#[must_use]
pub fn scope_holds(scope: &AccessScope, tenant: Uuid, id: Uuid) -> bool {
    if scope.is_unconstrained() {
        return true;
    }
    if scope.is_deny_all() {
        return false;
    }
    scope.constraints().iter().any(|constraint| {
        let filters = constraint.filters();
        !filters.is_empty()
            && filters
                .iter()
                .all(|filter| filter_holds(filter, tenant, id))
    })
}

fn filter_holds(filter: &toolkit_security::ScopeFilter, tenant: Uuid, id: Uuid) -> bool {
    use toolkit_security::pep_properties;
    if !filter.is_representable_in_memory() {
        return false;
    }
    let wanted = match filter.property() {
        pep_properties::OWNER_TENANT_ID => tenant,
        pep_properties::RESOURCE_ID => id,
        _ => return false,
    };
    filter
        .values()
        .iter()
        .any(|value| value.as_uuid() == Some(wanted))
}

pub(super) fn validation(field: &str, detail: impl Into<String>) -> DomainError {
    let mut r = ValidationReport::new();
    r.violate("VALIDATION", field, detail);
    DomainError::Validation(r)
}
pub fn conflict(code: &'static str, detail: impl Into<String>) -> TxError {
    TxError::Refused(DomainError::Conflict {
        code,
        detail: detail.into(),
    })
}
pub async fn find(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Sku, TxError> {
    repo::find_sku(tx, scope, tenant, id)
        .await
        .map_err(TxError::Repo)?
        .ok_or(TxError::Refused(DomainError::NotFound { what: "sku", id }))
}
/// The lifecycle an act's audit row records as its `to` (P-D-249). `before_next` is the SKU's
/// `lifecycle_next` before the act. When this act changed it, record the next it stored, or the
/// lifecycle in force when it cleared the next. Otherwise record the lifecycle in force, and
/// ignore a next an earlier act left.
pub async fn recorded_to(
    tx: &impl DBRunner,
    tenant: Uuid,
    id: Uuid,
    before_next: Option<LifecycleNext>,
) -> Result<bss_products_sdk::models::Lifecycle, TxError> {
    find(tx, &AccessScope::for_tenant(tenant), tenant, id)
        .await
        .map(|sku| recorded_lifecycle(before_next, &sku))
}
pub(super) fn recorded_lifecycle(
    before_next: Option<LifecycleNext>,
    sku: &Sku,
) -> bss_products_sdk::models::Lifecycle {
    if before_next == sku.lifecycle_next {
        sku.lifecycle
    } else {
        sku.lifecycle_next
            .map_or(sku.lifecycle, |next| next.lifecycle)
    }
}
pub(super) async fn resolve(
    state: &ApiState,
    _enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    content: &SkuContent,
) -> Result<Option<UsageRefAnswer>, CanonicalError> {
    let Some(reference) = content.usage_type_ref.as_deref() else {
        return Ok(None);
    };
    // P-D-232: a derived ref comes first, from this gear's own store, and the catalog is never
    // asked for it, configured or not.
    if derived::is_derived_ref(reference) {
        // The door already authorized this act. The version is the tenant's data; a second
        // `sku:read` check would refuse an approver whose grant is the unit alone (P-D-259).
        let Ok(meter) = bss_products_sdk::derived::MeterId::parse(reference) else {
            return Ok(Some(UsageRefAnswer::DerivedUnknown));
        };
        let scope = AccessScope::for_tenant(ctx.subject_tenant_id());
        let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
        let stored = super::derived_usage_types::stored_version(
            &conn,
            &scope,
            ctx.subject_tenant_id(),
            &meter,
        )
        .await
        .map_err(|e| super::derived_usage_types::stored_row_error(&e))?;
        return Ok(Some(match stored {
            Some((_, declaration)) => UsageRefAnswer::Derived(derived::DerivedPin {
                meter: meter.format(),
                output_unit: declaration.output_unit,
            }),
            None => UsageRefAnswer::DerivedUnknown,
        }));
    }
    // P-D-259: a raw ref is refused before the catalog is asked. A published raw SKU may still
    // move onto its identity wrapper (that proposal's ref is derived) or retire (no resolve).
    Err(derived::usage_type_required().into())
}
/// Maintenance never releases a pending unit's fence and compares the observed operation; a fence
/// it lifts is the system's act, with its audit row (P-D-213).
pub async fn expire(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    ttl: u32,
    now: OffsetDateTime,
) -> Result<(), TxError> {
    let cutoff = now - time::Duration::minutes(i64::from(ttl));
    if let Some(expired) = repo::expire_orphan_fence(tx, scope, tenant, id, cutoff)
        .await
        .map_err(TxError::Repo)?
    {
        expiry_audit(tx, tenant, expired, ttl, now).await?;
    }
    Ok(())
}
/// The audit row of an orphan fence the maintenance lifted (P-D-213): the system's act
/// (`repo::SYSTEM_ACTOR`) `sku.fence_expired` on the SKU, with the move it made and the TTL it
/// applied.
pub async fn expiry_audit(
    tx: &impl DBRunner,
    tenant: Uuid,
    expired: repo::ExpiredFence,
    ttl: u32,
    now: OffsetDateTime,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        &AccessScope::for_tenant(tenant),
        expiry_row(tenant, &expired, ttl, now),
        expired.id,
        Some(expired.revision),
    )
    .await
    .map_err(TxError::Repo)
}
/// The audit rows of every orphan fence one read's expiry lifted (P-D-213), as ONE multi-row
/// insert whatever their number (P-D-211): each row is [`expiry_audit`]'s.
pub async fn expiry_audits(
    tx: &impl DBRunner,
    tenant: Uuid,
    expired: &[repo::ExpiredFence],
    ttl: u32,
    now: OffsetDateTime,
) -> Result<(), TxError> {
    repo::write_eventless_act_audits(
        tx,
        tenant,
        expired
            .iter()
            .map(|e| (expiry_row(tenant, e, ttl, now), e.id, Some(e.revision)))
            .collect(),
    )
    .await
    .map_err(TxError::Repo)
}
/// An expiry's audit row: the system's act on the SKU, the move it made, the TTL it applied.
fn expiry_row(
    tenant: Uuid,
    expired: &repo::ExpiredFence,
    ttl: u32,
    now: OffsetDateTime,
) -> repo::AuditCommon {
    repo::AuditCommon {
        audit_id: Uuid::now_v7(),
        tenant_id: tenant,
        actor_ref: repo::SYSTEM_ACTOR,
        action: "sku.fence_expired".into(),
        subject_kind: "sku".into(),
        reason: Some(format!("fence_ttl_minutes={ttl}")),
        correlation_id: None,
        written_at: now,
        lifecycle: repo::LifecycleMove::between(expired.from, expired.to),
    }
}
pub(super) async fn touch(
    state: &ApiState,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(), CanonicalError> {
    let scope = scope.clone();
    let ttl = state.fence_ttl_minutes;
    state
        .db
        .db()
        .transaction_with_retry(
            super::category_tx_config(state),
            contention_db_err,
            move |tx| {
                let scope = scope.clone();
                Box::pin(async move {
                    expire(
                        tx,
                        &scope,
                        tenant,
                        id,
                        ttl,
                        crate::infra::storage::stored_now(),
                    )
                    .await
                })
            },
        )
        .await
        .map_err(tx_to_canonical)
}
/// Audit row identifiers belong to a separate aggregate from the authorized resource: the row is
/// written under the caller's own tenant, never a PDP-compiled scope, so the function takes none
/// (RS-53). `lifecycle` is the SKU lifecycle move the act made (P-D-213):
/// [`repo::LifecycleMove::NONE`] for an act on no SKU (the policy, a reference).
#[expect(
    clippy::too_many_arguments,
    reason = "Audit inputs explicitly bind subject and actor to the caller transaction"
)]
pub async fn audit(
    tx: &impl DBRunner,
    ctx: &SecurityContext,
    action: &str,
    kind: &str,
    id: Uuid,
    reason: Option<String>,
    now: OffsetDateTime,
    lifecycle: repo::LifecycleMove,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        &AccessScope::for_tenant(ctx.subject_tenant_id()),
        repo::AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: ctx.subject_tenant_id(),
            actor_ref: ctx.subject_id(),
            action: action.into(),
            subject_kind: kind.into(),
            reason,
            correlation_id: None,
            written_at: now,
            lifecycle,
        },
        id,
        None,
    )
    .await
    .map_err(TxError::Repo)
}
pub(super) async fn decided(
    outbox: &events::TxOutbox,
    tx: &DbTx<'_>,
    store: &repo::ProductsApprovalStore,
    unit: &Unit,
    actor: Uuid,
) -> Result<(), TxError> {
    let mut actors = store
        .decisions(tx, unit.id)
        .await?
        .into_iter()
        .filter(|d| !d.stale)
        .map(|d| d.actor)
        .collect::<Vec<_>>();
    actors.push(actor);
    actors.sort();
    actors.dedup();
    events::enqueue_typed(
        outbox,
        tx,
        broker::ApprovalUnitDecided {
            tenant_id: unit.tenant_id,
            unit_id: unit.id,
            kind: unit.kind.clone(),
            state: unit.state.as_str().into(),
            generation: unit.generation,
            actors,
        },
    )
    .await
    .map_err(TxError::from)
}

/// Retain the canonical violation and expose a numeric generation for reviewer clients.
pub(super) fn generation_problem(
    error: CanonicalError,
    generation: i32,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut problem = toolkit::api::canonical_prelude::Problem::from(error);
    problem.context["generation"] = serde_json::json!(generation);
    problem.into_response()
}

/// Policy reads use SETTINGS while retaining the read-side absent tenant hint.
pub(super) async fn settings_read(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    access_scope(
        enforcer,
        ctx,
        &resource_types::APPROVAL_UNIT,
        actions::SETTINGS,
        None,
    )
    .await
    .map_err(|e| {
        authz_error_to_canonical(e, |reason| {
            crate::infra::error_mapping::permission_denied(labels::APPROVAL_UNIT, reason)
        })
    })
}

#[cfg(test)]
#[path = "derived_binding_tests.rs"]
mod derived_binding_tests;

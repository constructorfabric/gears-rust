//! `CrossTenantGateway` — the forensic-gated cross-tenant read elevation (Slice
//! 6 Phase 2 Group 2C, architecture §8 / §G7).
//!
//! A routine audit read stays inside the caller's own (home) tenant. Opening a
//! DIFFERENT tenant's audit data is an elevated, forensic action: it requires an
//! explicit `targetScope`, an authorized role, AND an investigation reason. When
//! all three hold the gateway writes a `cross-tenant-access` secured-audit
//! record IN THE SAME TRANSACTION as the subsequent read — so the forensic trail
//! and the foreign read commit (or roll back) together. No reason, or an
//! unauthorized role, fails the request BEFORE any foreign row is read.
//!
//! The forensic record is written under a scope for the HOME tenant (the actor's
//! own tenant): the actor's tenant owns the cross-tenant-access record, and the
//! audit chain it links onto is the home tenant's chain. The returned
//! [`AccessScope`] is then the TARGET tenant's scope, which the caller binds to
//! the foreign read.

use std::sync::Arc;

use toolkit_db::DbError;
use toolkit_db::secure::{AccessScope, DbTx};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{LedgerMetricsPort, NoopLedgerMetrics};
use crate::infra::audit::event_type::AuditEventType;
use crate::infra::audit::store::SecuredAuditStore;
use crate::infra::posting::service::business;

/// The tenant a cross-tenant audit read targets (the `payerTenantId` / tenant
/// being opened). MVP carries just the `tenant_id`; a richer scope (legal
/// entity, period) is a future extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetScope {
    /// The tenant whose audit data is being opened.
    pub tenant_id: Uuid,
}

/// The cross-tenant elevation gateway. Holds the append-only
/// [`SecuredAuditStore`] (stateless); the forensic record is appended inside the
/// caller's transaction.
#[derive(Clone)]
pub struct CrossTenantGateway {
    audit: SecuredAuditStore,
    metrics: Arc<dyn LedgerMetricsPort>,
}

impl Default for CrossTenantGateway {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossTenantGateway {
    #[must_use]
    pub fn new() -> Self {
        Self {
            audit: SecuredAuditStore::new(),
            metrics: Arc::new(NoopLedgerMetrics),
        }
    }

    /// Bind the §9 metrics sink (`ledger_cross_tenant_access_total{reason_code}`
    /// is emitted on each successful elevation). Defaults to no-op until wired.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn LedgerMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Resolve the [`AccessScope`] a forensic audit read runs under, writing a
    /// `cross-tenant-access` record first when (and only when) the read crosses
    /// a tenant boundary. Runs inside the caller's `txn` so the record and the
    /// subsequent read share one transaction.
    ///
    /// Branch order (security-critical — keep it exactly):
    /// 1. **routine**: `target` is `None`, or names the home tenant → return the
    ///    home scope, write NO forensic record.
    /// 2. **denied**: a cross-tenant target with `role_authorized == false` →
    ///    [`DomainError::CrossTenantAccessDenied`] (before any read or write).
    /// 3. **missing-reason**: a cross-tenant target whose `reason` /
    ///    `reason_code` is missing or empty → [`DomainError::MissingInvestigationReason`]
    ///    (before any read or write — the forensic record is never half-written).
    /// 4. **elevated + record**: append the `cross-tenant-access` record under
    ///    the HOME tenant scope, then return the TARGET scope. A failed append
    ///    propagates and fails the whole request.
    ///
    /// # Errors
    /// A sentinel [`DbError`] carrying [`DomainError::CrossTenantAccessDenied`]
    /// (role) or [`DomainError::MissingInvestigationReason`] (reason); an
    /// infrastructure [`DbError`] when the forensic audit append fails (rolls the
    /// caller's transaction back).
    #[allow(
        clippy::too_many_arguments,
        reason = "the full elevation contract (home/target/role/actor/reason/reason_code/correlation) threaded into one txn-scoped decision"
    )]
    pub async fn resolve_read_scope(
        &self,
        txn: &DbTx<'_>,
        home_tenant: Uuid,
        target: Option<TargetScope>,
        role_authorized: bool,
        actor_ref: &str,
        reason: Option<&str>,
        reason_code: Option<&str>,
        correlation_id: Option<Uuid>,
    ) -> Result<AccessScope, DbError> {
        // 1. Routine: no target, or the target is the caller's own tenant. No
        //    forensic record — this is an ordinary same-tenant audit read.
        let Some(target) = target.filter(|t| t.tenant_id != home_tenant) else {
            return Ok(AccessScope::for_tenant(home_tenant));
        };

        // 2. Denied: the caller's role is not authorized to cross the boundary.
        //    Fail BEFORE any read or any forensic write.
        if !role_authorized {
            return Err(business(DomainError::CrossTenantAccessDenied(format!(
                "subject not authorized to open audit data for tenant {}",
                target.tenant_id
            ))));
        }

        // 3. Missing-reason: a cross-tenant read must carry an investigation
        //    reason (both a free-text `reason` and a machine `reason_code`).
        //    Fail BEFORE any read or any forensic write, so a reason-less request
        //    never leaves a half-written record.
        let reason = reason.map(str::trim).filter(|s| !s.is_empty());
        let reason_code = reason_code.map(str::trim).filter(|s| !s.is_empty());
        let (Some(reason), Some(reason_code)) = (reason, reason_code) else {
            return Err(business(DomainError::MissingInvestigationReason(format!(
                "a cross-tenant audit read of tenant {} requires both an \
                 X-Investigation-Reason and a reasonCode",
                target.tenant_id
            ))));
        };

        // 4. Elevated + record: append the forensic `cross-tenant-access` record
        //    under the HOME tenant scope (the actor's tenant owns the record and
        //    its chain), THEN return the TARGET scope for the foreign read. A
        //    failed append propagates and fails the whole request.
        let home_scope = AccessScope::for_tenant(home_tenant);
        let before_after = serde_json::json!({
            "targetScope": { "tenantId": target.tenant_id },
            "reason": reason,
        });
        self.audit
            .append(
                txn,
                &home_scope,
                home_tenant,
                AuditEventType::CrossTenantAccess,
                Some(actor_ref),
                Some(reason_code),
                &before_after,
                correlation_id,
                None,
            )
            // Propagate the append's `DbError` as-is — re-wrapping it (the old
            // `infra(format!(…))`) would bury a retryable SSI serialization
            // failure in a non-retryable `DbErr::Custom`.
            .await?;

        // §9: count the successful cross-tenant elevation, keyed by reason_code
        // (`ledger_cross_tenant_access_total{reason_code}`). Emitted only after
        // the forensic record committed in this txn, so the metric never
        // over-counts a rejected/half-written elevation.
        self.metrics.cross_tenant_access(reason_code);
        Ok(AccessScope::for_tenant(target.tenant_id))
    }
}

/// Resolve the DATA scope a forensic **write** action (erasure / re-identification)
/// runs against, applying the same cross-tenant elevation contract as
/// [`CrossTenantGateway::resolve_read_scope`] — but WITHOUT writing a
/// `cross-tenant-access` record. These actions write their own typed forensic
/// record (`erasure` / `re-identification`) inside their service, in the same
/// transaction as the mutation, so the `cross-tenant-access` event would be a
/// duplicate. Centralizing the decision here keeps the elevation contract — and
/// its branch order — in one place instead of open-coded per handler.
///
/// `role_authorized` is the caller's PEP decision for the TARGET tenant (the
/// handler computes it; the routine path passes `true`). `home_scope` is the
/// caller's own PEP-derived scope, returned unchanged for the routine path.
///
/// Branch order (security-critical — identical to the read gateway):
/// 1. **routine**: `target` is `None` or names the home tenant → `(home_tenant,
///    home_scope)`, no elevation.
/// 2. **denied**: cross-tenant target with `role_authorized == false` →
///    [`DomainError::CrossTenantAccessDenied`] (role checked BEFORE reason, §5).
/// 3. **missing-reason**: cross-tenant target with an empty/missing `reason` →
///    [`DomainError::MissingInvestigationReason`].
/// 4. **elevated**: `(target, AccessScope::for_tenant(target))`.
///
/// # Errors
/// [`DomainError::CrossTenantAccessDenied`] or [`DomainError::MissingInvestigationReason`].
pub fn resolve_action_scope(
    home_tenant: Uuid,
    home_scope: &AccessScope,
    target: Option<TargetScope>,
    role_authorized: bool,
    reason: Option<&str>,
) -> Result<(Uuid, AccessScope), DomainError> {
    let Some(target) = target.filter(|t| t.tenant_id != home_tenant) else {
        return Ok((home_tenant, home_scope.clone()));
    };
    if !role_authorized {
        return Err(DomainError::CrossTenantAccessDenied(format!(
            "subject not authorized to open tenant {} for this action",
            target.tenant_id
        )));
    }
    if reason.is_none_or(|s| s.trim().is_empty()) {
        return Err(DomainError::MissingInvestigationReason(format!(
            "a cross-tenant action on tenant {} requires an X-Investigation-Reason",
            target.tenant_id
        )));
    }
    Ok((target.tenant_id, AccessScope::for_tenant(target.tenant_id)))
}

/// Routine subtree read scope — the DEGRADED FALLBACK seam (architecture §8 /
/// §G7). Today it returns the caller's OWN tenant scope only: the "resolver
/// unavailable → own tenant only, flagged degraded" behavior. The drop-in future
/// is a `TenantResolverClient` (in `BarrierMode::Respect`) that expands the home
/// tenant to its authorized subtree; that wiring is deliberately omitted here to
/// keep the gear hermetic (no `tenant-resolver` dependency in tests).
#[must_use]
pub fn subtree_read_scope(home_tenant: Uuid) -> AccessScope {
    AccessScope::for_tenant(home_tenant)
}

#[cfg(test)]
#[path = "cross_tenant_tests.rs"]
mod cross_tenant_tests;

//! Tenancy and authorization (DESIGN §4.8, T-D-70; feature 01 §3.1, §5.2).
//!
//! The GTS resource types and actions, the operation matrix that SDK, REST, consumers and
//! workers apply alike, the PEP gate over `PolicyEnforcer` (deny by default), selector narrowing
//! and the service grant background work binds per tenant. Every repository call runs with the
//! `AccessScope` this module returns; no query runs without one.

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-authz:p1
// @cpt-algo:cpt-cf-bss-rating-algo-foundation-authorize:p1

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, SecurityContext, pep_properties,
};
use uuid::Uuid;

use crate::domain::error::{Cause, DomainError};
use crate::domain::ports::TenantGrant;

/// The GTS resource types Rating registers (DESIGN §4.8).
pub mod labels {
    use toolkit_gts::gts_id;
    /// Usage records, exceptions, inbox entries and source losses.
    pub const USAGE: &str = gts_id!("cf.bss.rating.usage.v1~");
    /// Children, window and fact results, deliveries and snapshots.
    pub const RESULT: &str = gts_id!("cf.bss.rating.result.v1~");
    /// Administrative re-rate runs.
    pub const RERATE: &str = gts_id!("cf.bss.rating.rerate.v1~");
    /// Pre-purchase order evaluation.
    pub const EVALUATION: &str = gts_id!("cf.bss.rating.evaluation.v1~");
    /// Seller finalization policies and their `delay_only` acceptances.
    pub const FINALIZATION_POLICY: &str = gts_id!("cf.bss.rating.finalization_policy.v1~");
    /// Platform-default finalization policies.
    pub const PLATFORM_FINALIZATION_POLICY: &str =
        gts_id!("cf.bss.rating.platform_finalization_policy.v1~");
    /// Source-loss resolution.
    pub const SOURCE_LOSS: &str = gts_id!("cf.bss.rating.source_loss.v1~");
    /// Retention holds.
    pub const RETENTION_HOLD: &str = gts_id!("cf.bss.rating.retention_hold.v1~");
    /// Every label, in catalog order.
    pub const ALL: &[&str] = &[
        USAGE,
        RESULT,
        RERATE,
        EVALUATION,
        FINALIZATION_POLICY,
        PLATFORM_FINALIZATION_POLICY,
        SOURCE_LOSS,
        RETENTION_HOLD,
    ];
}

/// The actions of the catalog (DESIGN §4.8).
pub mod actions {
    pub const READ: &str = "read";
    pub const RETRY: &str = "retry";
    pub const RELEASE: &str = "release";
    pub const EXECUTE: &str = "execute";
    pub const EVALUATE: &str = "evaluate";
    pub const WRITE: &str = "write";
    pub const ACCEPT_DELAY_ONLY: &str = "accept_delay_only";
    pub const RESOLVE: &str = "resolve";
    pub const APPROVE: &str = "approve";
    pub const PLACE: &str = "place";
}

/// Every Rating resource is scoped by its owning tenant and its row identity.
pub const SUPPORTED_PROPERTIES: &[&str] =
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// Resource descriptors that bind PDP constraints to the supported properties.
pub mod resource_types {
    use super::{ResourceType, SUPPORTED_PROPERTIES, labels};
    pub const USAGE: ResourceType = ResourceType::from_static(labels::USAGE, SUPPORTED_PROPERTIES);
    pub const RESULT: ResourceType =
        ResourceType::from_static(labels::RESULT, SUPPORTED_PROPERTIES);
    pub const RERATE: ResourceType =
        ResourceType::from_static(labels::RERATE, SUPPORTED_PROPERTIES);
    pub const EVALUATION: ResourceType =
        ResourceType::from_static(labels::EVALUATION, SUPPORTED_PROPERTIES);
    pub const FINALIZATION_POLICY: ResourceType =
        ResourceType::from_static(labels::FINALIZATION_POLICY, SUPPORTED_PROPERTIES);
    pub const PLATFORM_FINALIZATION_POLICY: ResourceType =
        ResourceType::from_static(labels::PLATFORM_FINALIZATION_POLICY, SUPPORTED_PROPERTIES);
    pub const SOURCE_LOSS: ResourceType =
        ResourceType::from_static(labels::SOURCE_LOSS, SUPPORTED_PROPERTIES);
    pub const RETENTION_HOLD: ResourceType =
        ResourceType::from_static(labels::RETENTION_HOLD, SUPPORTED_PROPERTIES);
}

/// The operation matrix (DESIGN §3.3 operator REST plane, §4.8): every Rating operation with
/// its PDP check and whether it is audited. SDK methods, REST routes, consumers and workers name
/// their operation here, so one table decides them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// `find_runs`, `get_run` (`bss_rating.runs.get`).
    RunsGet,
    /// `window_results` (`bss_rating.windows.list`).
    WindowsList,
    /// `deliveries_since` (SDK only).
    DeliveriesSince,
    /// `bss_rating.snapshots.get`.
    SnapshotsGet,
    /// `bss_rating.exceptions.list`.
    ExceptionsList,
    /// `bss_rating.exceptions.retry`.
    ExceptionsRetry,
    /// `bss_rating.inbox.release`.
    InboxRelease,
    /// `bss_rating.reratings.create`.
    ReratingsCreate,
    /// `bss_rating.reratings.get`.
    ReratingsGet,
    /// `bss_rating.reratings.cancel`.
    ReratingsCancel,
    /// `OrderEvaluationV1::evaluate`.
    OrderEvaluate,
    /// `bss_rating.finalization_policies.list`.
    FinalizationPoliciesList,
    /// `bss_rating.finalization_policies.create`.
    FinalizationPoliciesCreate,
    /// `bss_rating.finalization_policies.accept_delay_only`.
    FinalizationPoliciesAcceptDelayOnly,
    /// Reading the platform defaults.
    PlatformFinalizationPoliciesRead,
    /// `bss_rating.platform_finalization_policies.create`.
    PlatformFinalizationPoliciesCreate,
    /// `bss_rating.platform_finalization_policies.accept_delay_only`.
    PlatformFinalizationPoliciesAcceptDelayOnly,
    /// `bss_rating.source_losses.list`.
    SourceLossesList,
    /// `bss_rating.source_losses.submit_reconciliation`.
    SourceLossesSubmitReconciliation,
    /// `bss_rating.source_losses.resolve`.
    SourceLossesResolve,
    /// `bss_rating.source_losses.approve`.
    SourceLossesApprove,
    /// `bss_rating.source_losses.reject`.
    SourceLossesReject,
    /// `bss_rating.retention_holds.list`.
    RetentionHoldsList,
    /// `bss_rating.retention_holds.create`.
    RetentionHoldsCreate,
    /// `bss_rating.retention_holds.release`.
    RetentionHoldsRelease,
    /// Rating's own background work and intake for one tenant: the service grant.
    ServiceWork,
}

impl Operation {
    /// Every operation of the matrix.
    pub const ALL: [Self; 26] = [
        Self::RunsGet,
        Self::WindowsList,
        Self::DeliveriesSince,
        Self::SnapshotsGet,
        Self::ExceptionsList,
        Self::ExceptionsRetry,
        Self::InboxRelease,
        Self::ReratingsCreate,
        Self::ReratingsGet,
        Self::ReratingsCancel,
        Self::OrderEvaluate,
        Self::FinalizationPoliciesList,
        Self::FinalizationPoliciesCreate,
        Self::FinalizationPoliciesAcceptDelayOnly,
        Self::PlatformFinalizationPoliciesRead,
        Self::PlatformFinalizationPoliciesCreate,
        Self::PlatformFinalizationPoliciesAcceptDelayOnly,
        Self::SourceLossesList,
        Self::SourceLossesSubmitReconciliation,
        Self::SourceLossesResolve,
        Self::SourceLossesApprove,
        Self::SourceLossesReject,
        Self::RetentionHoldsList,
        Self::RetentionHoldsCreate,
        Self::RetentionHoldsRelease,
        Self::ServiceWork,
    ];

    /// The resource type and action the PDP is asked about.
    ///
    /// `ServiceWork` asks `usage × read` for Rating's service identity in the work item's tenant:
    /// the catalog names no service action, and reading the tenant's usage-plane data is the
    /// least grant every background path needs.
    #[must_use]
    pub fn check(self) -> (ResourceType, &'static str) {
        use resource_types as rt;
        match self {
            Self::RunsGet | Self::WindowsList | Self::DeliveriesSince | Self::SnapshotsGet => {
                (rt::RESULT, actions::READ)
            }
            Self::ExceptionsList | Self::SourceLossesList | Self::ServiceWork => {
                (rt::USAGE, actions::READ)
            }
            Self::ExceptionsRetry => (rt::USAGE, actions::RETRY),
            Self::InboxRelease => (rt::USAGE, actions::RELEASE),
            Self::ReratingsCreate | Self::ReratingsCancel => (rt::RERATE, actions::EXECUTE),
            Self::ReratingsGet => (rt::RERATE, actions::READ),
            Self::OrderEvaluate => (rt::EVALUATION, actions::EVALUATE),
            Self::FinalizationPoliciesList => (rt::FINALIZATION_POLICY, actions::READ),
            Self::FinalizationPoliciesCreate => (rt::FINALIZATION_POLICY, actions::WRITE),
            Self::FinalizationPoliciesAcceptDelayOnly => {
                (rt::FINALIZATION_POLICY, actions::ACCEPT_DELAY_ONLY)
            }
            Self::PlatformFinalizationPoliciesRead => {
                (rt::PLATFORM_FINALIZATION_POLICY, actions::READ)
            }
            Self::PlatformFinalizationPoliciesCreate => {
                (rt::PLATFORM_FINALIZATION_POLICY, actions::WRITE)
            }
            Self::PlatformFinalizationPoliciesAcceptDelayOnly => {
                (rt::PLATFORM_FINALIZATION_POLICY, actions::ACCEPT_DELAY_ONLY)
            }
            Self::SourceLossesSubmitReconciliation | Self::SourceLossesResolve => {
                (rt::SOURCE_LOSS, actions::RESOLVE)
            }
            Self::SourceLossesApprove | Self::SourceLossesReject => {
                (rt::SOURCE_LOSS, actions::APPROVE)
            }
            Self::RetentionHoldsList => (rt::RETENTION_HOLD, actions::READ),
            Self::RetentionHoldsCreate => (rt::RETENTION_HOLD, actions::PLACE),
            Self::RetentionHoldsRelease => (rt::RETENTION_HOLD, actions::RELEASE),
        }
    }

    /// Whether the operation writes an operator audit row: `retry`, `release`, `execute`,
    /// `resolve`, `approve`, hold `place` / `release`, `accept_delay_only` and both policy `write`
    /// actions (DESIGN §4.8).
    #[must_use]
    pub const fn audited(self) -> bool {
        matches!(
            self,
            Self::ExceptionsRetry
                | Self::InboxRelease
                | Self::ReratingsCreate
                | Self::ReratingsCancel
                | Self::FinalizationPoliciesCreate
                | Self::FinalizationPoliciesAcceptDelayOnly
                | Self::PlatformFinalizationPoliciesCreate
                | Self::PlatformFinalizationPoliciesAcceptDelayOnly
                | Self::SourceLossesSubmitReconciliation
                | Self::SourceLossesResolve
                | Self::SourceLossesApprove
                | Self::SourceLossesReject
                | Self::RetentionHoldsCreate
                | Self::RetentionHoldsRelease
        )
    }

    /// The operation id recorded in audit rows and registered on REST routes.
    #[must_use]
    pub const fn operation_id(self) -> &'static str {
        match self {
            Self::RunsGet => "bss_rating.runs.get",
            Self::WindowsList => "bss_rating.windows.list",
            Self::DeliveriesSince => "bss_rating.deliveries.since",
            Self::SnapshotsGet => "bss_rating.snapshots.get",
            Self::ExceptionsList => "bss_rating.exceptions.list",
            Self::ExceptionsRetry => "bss_rating.exceptions.retry",
            Self::InboxRelease => "bss_rating.inbox.release",
            Self::ReratingsCreate => "bss_rating.reratings.create",
            Self::ReratingsGet => "bss_rating.reratings.get",
            Self::ReratingsCancel => "bss_rating.reratings.cancel",
            Self::OrderEvaluate => "bss_rating.order_evaluation.evaluate",
            Self::FinalizationPoliciesList => "bss_rating.finalization_policies.list",
            Self::FinalizationPoliciesCreate => "bss_rating.finalization_policies.create",
            Self::FinalizationPoliciesAcceptDelayOnly => {
                "bss_rating.finalization_policies.accept_delay_only"
            }
            Self::PlatformFinalizationPoliciesRead => {
                "bss_rating.platform_finalization_policies.read"
            }
            Self::PlatformFinalizationPoliciesCreate => {
                "bss_rating.platform_finalization_policies.create"
            }
            Self::PlatformFinalizationPoliciesAcceptDelayOnly => {
                "bss_rating.platform_finalization_policies.accept_delay_only"
            }
            Self::SourceLossesList => "bss_rating.source_losses.list",
            Self::SourceLossesSubmitReconciliation => {
                "bss_rating.source_losses.submit_reconciliation"
            }
            Self::SourceLossesResolve => "bss_rating.source_losses.resolve",
            Self::SourceLossesApprove => "bss_rating.source_losses.approve",
            Self::SourceLossesReject => "bss_rating.source_losses.reject",
            Self::RetentionHoldsList => "bss_rating.retention_holds.list",
            Self::RetentionHoldsCreate => "bss_rating.retention_holds.create",
            Self::RetentionHoldsRelease => "bss_rating.retention_holds.release",
            Self::ServiceWork => "bss_rating.service.work",
        }
    }
}

/// The subject type of Rating's service identity (`bss-rating.system`, pricing D-424).
pub const SERVICE_SUBJECT_TYPE: &str = "bss-rating.system";

/// The subject id of Rating's service identity.
pub const SERVICE_SUBJECT_ID: Uuid = Uuid::from_u128(0x5c1d_7e2a_4b3f_5a6e_8d9c_0b1a_2f3e_4d5c);

/// Rating's service identity acting for `tenant_id`: one `SecurityContext` per work item, with
/// the item's tenant as the subject tenant (Orders D-160 pattern).
///
/// # Errors
///
/// [`DomainError::AuthzUnavailable`] if the context cannot be built.
pub fn service_context(tenant_id: Uuid) -> Result<SecurityContext, DomainError> {
    SecurityContext::builder()
        .subject_id(SERVICE_SUBJECT_ID)
        .subject_tenant_id(tenant_id)
        .subject_type(SERVICE_SUBJECT_TYPE)
        .build()
        .map_err(|e| {
            DomainError::AuthzUnavailable(format!("service identity: {e}"), Some(Cause::new(e)))
        })
}

/// The PEP gate: asks the PDP whether `operation` is permitted for `ctx` and returns the compiled
/// `AccessScope`, the SQL filter of every repository call that follows.
///
/// - **Deny by default**: a denial, or constraints that do not compile, refuse with
///   [`DomainError::PermissionDenied`]; a PDP outage refuses with
///   [`DomainError::AuthzUnavailable`]. Constraints are always required, so an unconstrained
///   allow fails closed instead of leaking every tenant.
/// - `owner_tenant` names the tenant a write lands in; the returned scope must contain it, so a
///   degraded decision cannot authorize a cross-tenant write. Reads pass `None` and use the scope.
/// - Every request asks for the caller's own tenant (`TenantMode::RootOnly`), as Pricing and
///   Products do; delegated administration is whatever the PDP grants beyond that.
///
/// # Errors
///
/// As above.
pub async fn authorize(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    operation: Operation,
    owner_tenant: Option<Uuid>,
) -> Result<AccessScope, DomainError> {
    // @cpt-begin:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-pdp
    let (resource, action) = operation.check();
    let mut request = AccessRequest::new()
        .require_constraints(true)
        .tenant_mode(TenantMode::RootOnly);
    if let Some(tenant) = owner_tenant {
        request = request.resource_property(pep_properties::OWNER_TENANT_ID, tenant);
    }
    let denial = |reason: String, pdp_reason: Option<&authz_resolver_sdk::models::DenyReason>| {
        tracing::warn!(
            target: "bss_rating.authz.deny",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            operation = operation.operation_id(),
            owner_tenant = ?owner_tenant,
            reason = %reason,
            deny_code = pdp_reason.map(|r| r.error_code.as_str()),
            deny_details = pdp_reason.and_then(|r| r.details.as_deref()),
            "bss-rating: authorization denied"
        );
        DomainError::PermissionDenied(reason)
    };
    let scope = enforcer
        .access_scope_with(ctx, &resource, action, None, &request)
        .await
        .map_err(|e| match e {
            authz_resolver_sdk::EnforcerError::Denied { ref deny_reason } => {
                denial(e.to_string(), deny_reason.as_ref())
            }
            authz_resolver_sdk::EnforcerError::CompileFailed(ref compile) => {
                tracing::warn!(
                    target: "bss_rating.authz.deny",
                    operation = operation.operation_id(),
                    error = %compile,
                    "bss-rating: authz constraints did not compile"
                );
                denial(CONSTRAINT_COMPILATION_FAILED.to_owned(), None)
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                tracing::warn!(
                    target: "bss_rating.authz",
                    operation = operation.operation_id(),
                    error = %e,
                    "bss-rating: the PDP could not be evaluated"
                );
                DomainError::AuthzUnavailable(
                    "PDP evaluation failed".to_owned(),
                    Some(Cause::new(e)),
                )
            }
        })?;
    if let Some(target) = owner_tenant
        && !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target)
    {
        return Err(denial(
            format!("subject not authorized for resources owned by tenant {target}"),
            None,
        ));
    }
    // @cpt-begin:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-return
    Ok(scope)
    // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-return
    // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-pdp
}

/// The reason a denial carries when the PDP's constraints did not compile.
pub const CONSTRAINT_COMPILATION_FAILED: &str = "constraint_compilation_failed";

/// How a tenant selector outside the granted scope is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorRead {
    /// A collection read: `403 PermissionDenied`.
    Collection,
    /// An object read: `404`, indistinguishable from an absent object.
    Object,
}

/// Narrows `scope` to `tenant_id`, a tenant argument or filter of the request. A selector only
/// narrows the PDP-granted scope, never widens or replaces it: each access path of the scope is
/// kept with its tenant filter intersected with the selector, and a path that cannot admit the
/// tenant is dropped.
///
/// # Errors
///
/// For a tenant outside `scope`: [`DomainError::PermissionDenied`] on a collection read,
/// [`DomainError::NotFound`] on an object read. An unconstrained `scope` is refused with
/// [`DomainError::PermissionDenied`]: [`authorize`] always requires constraints, so only a scope
/// the PDP did not decide can be unconstrained, and narrowing it would invent a grant.
pub fn narrow_to_tenant(
    scope: &AccessScope,
    tenant_id: Uuid,
    read: SelectorRead,
) -> Result<AccessScope, DomainError> {
    // @cpt-begin:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-selector
    if scope.is_unconstrained() {
        return Err(DomainError::PermissionDenied(
            "an unconstrained scope is not one the PDP decided".to_owned(),
        ));
    }
    if !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_id) {
        return Err(outside(tenant_id, read));
    }
    let selector = ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]);
    // Keep the access paths that admit the tenant, each with its tenant filters replaced by the
    // selector; a filter that cannot be decided in memory (a tenant subtree) stays beside the
    // selector, so the database ANDs both.
    let narrowed: Vec<ScopeConstraint> = scope
        .constraints()
        .iter()
        .filter_map(|constraint| {
            let mut filters = vec![selector.clone()];
            for filter in constraint.filters() {
                if filter.property() != pep_properties::OWNER_TENANT_ID
                    || !filter.is_representable_in_memory()
                {
                    filters.push(filter.clone());
                } else if !filter.uuid_values().contains(&tenant_id) {
                    return None;
                }
            }
            Some(ScopeConstraint::new(filters))
        })
        .collect();
    if narrowed.is_empty() {
        return Err(outside(tenant_id, read));
    }
    Ok(AccessScope::from_constraints(narrowed))
    // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-selector
}

fn outside(tenant_id: Uuid, read: SelectorRead) -> DomainError {
    match read {
        SelectorRead::Collection => DomainError::PermissionDenied(format!(
            "tenant {tenant_id} lies outside the caller's scope"
        )),
        SelectorRead::Object => DomainError::NotFound { what: "object" },
    }
}

/// Rating's service grant (DESIGN §4.8): the PDP's answer for the service identity in one tenant.
/// Background work and intake bind the scope it returns for every repository call of that tenant.
#[derive(Clone)]
pub struct ServiceGrant {
    enforcer: PolicyEnforcer,
}

impl ServiceGrant {
    /// The grant as `enforcer` evaluates it.
    #[must_use]
    pub fn new(enforcer: PolicyEnforcer) -> Self {
        Self { enforcer }
    }
}

#[async_trait]
impl TenantGrant for ServiceGrant {
    async fn scope_for(&self, tenant_id: Uuid) -> Result<AccessScope, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-worker
        let ctx = service_context(tenant_id)?;
        let scope = authorize(
            &self.enforcer,
            &ctx,
            Operation::ServiceWork,
            Some(tenant_id),
        )
        .await
        .map_err(|e| match e {
            DomainError::PermissionDenied(_) => DomainError::OutsideGrant { tenant_id },
            other => other,
        })?;
        narrow_to_tenant(&scope, tenant_id, SelectorRead::Collection)
            .map_err(|_| DomainError::OutsideGrant { tenant_id })
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-worker
    }
}

fn authz_type_schema_json(gts_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
    })
}

/// Stub type schemas of every resource label, registered in the types registry at `init` so RBAC
/// roles can target them; a refused registration fails the boot.
#[must_use]
pub fn authz_label_type_schemas() -> Vec<serde_json::Value> {
    labels::ALL
        .iter()
        .map(|label| authz_type_schema_json(label, &format!("BSS Rating authz label {label}")))
        .collect()
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod tests;

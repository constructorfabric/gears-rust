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

/// The resource an operation is about: one per GTS label (DESIGN §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// [`labels::USAGE`].
    Usage,
    /// [`labels::RESULT`].
    Result,
    /// [`labels::RERATE`].
    Rerate,
    /// [`labels::EVALUATION`].
    Evaluation,
    /// [`labels::FINALIZATION_POLICY`].
    FinalizationPolicy,
    /// [`labels::PLATFORM_FINALIZATION_POLICY`].
    PlatformFinalizationPolicy,
    /// [`labels::SOURCE_LOSS`].
    SourceLoss,
    /// [`labels::RETENTION_HOLD`].
    RetentionHold,
}

impl Resource {
    /// The PEP descriptor the PDP is asked about.
    #[must_use]
    pub const fn resource_type(self) -> ResourceType {
        match self {
            Self::Usage => resource_types::USAGE,
            Self::Result => resource_types::RESULT,
            Self::Rerate => resource_types::RERATE,
            Self::Evaluation => resource_types::EVALUATION,
            Self::FinalizationPolicy => resource_types::FINALIZATION_POLICY,
            Self::PlatformFinalizationPolicy => resource_types::PLATFORM_FINALIZATION_POLICY,
            Self::SourceLoss => resource_types::SOURCE_LOSS,
            Self::RetentionHold => resource_types::RETENTION_HOLD,
        }
    }
}

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
/// their operation here, so one table decides them all. Every property is an exhaustive match, so
/// a new operation does not compile until each is decided for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::VariantArray)]
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
    /// Every operation of the matrix, derived from the enum so none is left out.
    pub const ALL: &[Self] = <Self as strum::VariantArray>::VARIANTS;

    /// The resource and action the PDP is asked about.
    ///
    /// `ServiceWork` asks `usage × read` for Rating's service identity in the work item's tenant:
    /// the catalog names no service action, and reading the tenant's usage-plane data is the
    /// least grant every background path needs.
    #[must_use]
    pub fn check(self) -> (ResourceType, &'static str) {
        (self.resource().resource_type(), self.action())
    }

    /// The resource the operation is about.
    #[must_use]
    pub const fn resource(self) -> Resource {
        match self {
            Self::RunsGet | Self::WindowsList | Self::DeliveriesSince | Self::SnapshotsGet => {
                Resource::Result
            }
            Self::ExceptionsList
            | Self::ExceptionsRetry
            | Self::InboxRelease
            | Self::SourceLossesList
            | Self::ServiceWork => Resource::Usage,
            Self::ReratingsCreate | Self::ReratingsGet | Self::ReratingsCancel => Resource::Rerate,
            Self::OrderEvaluate => Resource::Evaluation,
            Self::FinalizationPoliciesList
            | Self::FinalizationPoliciesCreate
            | Self::FinalizationPoliciesAcceptDelayOnly => Resource::FinalizationPolicy,
            Self::PlatformFinalizationPoliciesRead
            | Self::PlatformFinalizationPoliciesCreate
            | Self::PlatformFinalizationPoliciesAcceptDelayOnly => {
                Resource::PlatformFinalizationPolicy
            }
            Self::SourceLossesSubmitReconciliation
            | Self::SourceLossesResolve
            | Self::SourceLossesApprove
            | Self::SourceLossesReject => Resource::SourceLoss,
            Self::RetentionHoldsList | Self::RetentionHoldsCreate | Self::RetentionHoldsRelease => {
                Resource::RetentionHold
            }
        }
    }

    /// The action the PDP is asked about.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::RunsGet
            | Self::WindowsList
            | Self::DeliveriesSince
            | Self::SnapshotsGet
            | Self::ExceptionsList
            | Self::SourceLossesList
            | Self::ServiceWork
            | Self::ReratingsGet
            | Self::FinalizationPoliciesList
            | Self::PlatformFinalizationPoliciesRead
            | Self::RetentionHoldsList => actions::READ,
            Self::ExceptionsRetry => actions::RETRY,
            Self::InboxRelease | Self::RetentionHoldsRelease => actions::RELEASE,
            Self::ReratingsCreate | Self::ReratingsCancel => actions::EXECUTE,
            Self::OrderEvaluate => actions::EVALUATE,
            Self::FinalizationPoliciesCreate | Self::PlatformFinalizationPoliciesCreate => {
                actions::WRITE
            }
            Self::FinalizationPoliciesAcceptDelayOnly
            | Self::PlatformFinalizationPoliciesAcceptDelayOnly => actions::ACCEPT_DELAY_ONLY,
            Self::SourceLossesSubmitReconciliation | Self::SourceLossesResolve => actions::RESOLVE,
            Self::SourceLossesApprove | Self::SourceLossesReject => actions::APPROVE,
            Self::RetentionHoldsCreate => actions::PLACE,
        }
    }

    /// Whether the operation writes an operator audit row: `retry`, `release`, `execute`,
    /// `resolve`, `approve`, hold `place` / `release`, `accept_delay_only` and both policy `write`
    /// actions (DESIGN §4.8).
    #[must_use]
    pub const fn audited(self) -> bool {
        match self {
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
            | Self::RetentionHoldsRelease => true,
            Self::RunsGet
            | Self::WindowsList
            | Self::DeliveriesSince
            | Self::SnapshotsGet
            | Self::ExceptionsList
            | Self::ReratingsGet
            | Self::OrderEvaluate
            | Self::FinalizationPoliciesList
            | Self::PlatformFinalizationPoliciesRead
            | Self::SourceLossesList
            | Self::RetentionHoldsList
            | Self::ServiceWork => false,
        }
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
/// the item's tenant as the subject tenant. Interim (DESIGN §4.8, T-D-83): an authenticated
/// per-tenant service principal replaces this construction, as Orders' D-194 replaced its D-160;
/// until then the grant is only as strong as the deployment's policy for `bss-rating.system`.
///
/// # Errors
///
/// [`DomainError::Internal`] if the context cannot be built: every field the builder needs is
/// set here, so a refusal is a fault of Rating's, never a PDP outage to retry.
pub fn service_context(tenant_id: Uuid) -> Result<SecurityContext, DomainError> {
    SecurityContext::builder()
        .subject_id(SERVICE_SUBJECT_ID)
        .subject_tenant_id(tenant_id)
        .subject_type(SERVICE_SUBJECT_TYPE)
        .build()
        .map_err(|e| DomainError::Internal(format!("service identity: {e}"), Some(Cause::new(e))))
}

/// The PEP gate: asks the PDP whether `operation` is permitted for `ctx` and returns the compiled
/// `AccessScope`, the SQL filter of every repository call that follows.
///
/// - **Deny by default**: a denial, or constraints that do not compile, refuse with
///   [`DomainError::PermissionDenied`]; a PDP outage refuses with
///   [`DomainError::AuthzUnavailable`]. Constraints are always required, so an unconstrained
///   allow fails closed instead of leaking every tenant.
/// - `owner_tenant` names the tenant a write lands in; the returned scope must admit a write
///   there ([`admits_write`]), so a degraded decision cannot authorize a cross-tenant write. Reads
///   pass `None` and use the scope.
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
        && !admits_write(&scope, target)
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

/// Whether `scope` admits writing a row owned by `tenant_id`, as far as Rating can decide it:
/// some access path has tenant filters, and each is a list that holds the tenant. The path's
/// other filters are `SecureORM`'s to evaluate, which it does for the whole path at the write.
///
/// A path with a tenant-subtree filter never counts. Subtree membership cannot be decided in
/// memory, and `SecureORM`'s insert validation compares a row with a filter's listed values, so it
/// refuses such a write too; a write under a subtree grant fails closed here instead of
/// mid-transaction, until the platform validates writes against the tenant tree. Every Rating
/// request asks the PDP for `TenantMode::RootOnly`, which answers with tenant lists.
#[must_use]
pub fn admits_write(scope: &AccessScope, tenant_id: Uuid) -> bool {
    scope.constraints().iter().any(|constraint| {
        let mut tenant_filters = constraint
            .filters()
            .iter()
            .filter(|filter| filter.property() == pep_properties::OWNER_TENANT_ID)
            .peekable();
        tenant_filters.peek().is_some()
            && tenant_filters.all(|filter| {
                filter.is_representable_in_memory() && filter.uuid_values().contains(&tenant_id)
            })
    })
}

/// [`admits_write`] as a refusal: [`DomainError::PermissionDenied`], logged, when `scope` does
/// not admit writing `operation`'s row in `tenant_id`.
///
/// # Errors
///
/// As above.
pub fn require_write(
    scope: &AccessScope,
    tenant_id: Uuid,
    operation: Operation,
) -> Result<(), DomainError> {
    if admits_write(scope, tenant_id) {
        return Ok(());
    }
    tracing::warn!(
        target: "bss_rating.authz.deny",
        operation = operation.operation_id(),
        tenant_id = %tenant_id,
        "bss-rating: the scope does not admit a write in the tenant"
    );
    Err(DomainError::PermissionDenied(format!(
        "subject not authorized to write resources owned by tenant {tenant_id}"
    )))
}

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
/// tenant is dropped. A tenant filter that cannot be decided in memory (a tenant subtree) stays
/// beside the selector for the database to evaluate, so a subtree grant narrows for reads as a
/// tenant list does; a write needs [`admits_write`] as well.
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
            DomainError::PermissionDenied(_) => outside_grant(tenant_id, &e),
            other => other,
        })?;
        // `authorize` already checked that the scope holds the tenant, so a refusal here means
        // the PDP answered with a shape it should not (an unconstrained scope, say); it is logged
        // as that, not only as a tenant without a grant.
        narrow_to_tenant(&scope, tenant_id, SelectorRead::Collection)
            .map_err(|e| outside_grant(tenant_id, &e))
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-authorize:p1:inst-az-worker
    }
}

/// A tenant the service grant refuses, with the refusal behind it logged: the caller sees only
/// [`DomainError::OutsideGrant`].
fn outside_grant(tenant_id: Uuid, refusal: &DomainError) -> DomainError {
    tracing::warn!(
        target: "bss_rating.authz.deny",
        tenant_id = %tenant_id,
        refusal = %refusal,
        "bss-rating: the service grant does not cover the tenant"
    );
    DomainError::OutsideGrant { tenant_id }
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

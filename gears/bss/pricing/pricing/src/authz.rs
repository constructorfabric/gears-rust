//! Pricing resource labels and the deny-by-default PEP gate.

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{AccessScope, ScopeFilter, SecurityContext, pep_properties};
use uuid::Uuid;

/// Concrete PDP-visible pricing resources.
pub mod labels {
    use toolkit_gts::gts_id;
    pub const PRICE_BOOK: &str = gts_id!("cf.bss.pricing.price_book.v1~");
    pub const PRICE_BOOK_ENTRY: &str = gts_id!("cf.bss.pricing.price_book_entry.v1~");
    pub const PRICE: &str = gts_id!("cf.bss.pricing.price.v1~");
    pub const APPROVAL_UNIT: &str = gts_id!("cf.bss.pricing.approval_unit.v1~");
    pub const CONFIG: &str = gts_id!("cf.bss.pricing.config.v1~");
    /// A plan, its revisions and their items: one label, read and author (phase 3), and submit
    /// for a revision's approval (D-418), as `price:submit` is for a price.
    pub const PLAN: &str = gts_id!("cf.bss.pricing.plan.v1~");
    pub const ACCEPTANCE: &str = gts_id!("cf.bss.pricing.acceptance.v1~");
    pub const ALL: &[&str] = &[
        PRICE_BOOK,
        PRICE_BOOK_ENTRY,
        PRICE,
        APPROVAL_UNIT,
        CONFIG,
        PLAN,
        ACCEPTANCE,
    ];
}
/// Independent authoring and governance actions.
pub mod actions {
    pub const CREATE: &str = "create";
    pub const HOLD: &str = "hold";
    pub const READ: &str = "read";
    pub const AUTHOR: &str = "author";
    pub const SUBMIT: &str = "submit";
    pub const APPROVE: &str = "approve";
    pub const SETTINGS: &str = "settings";
}
/// Resource descriptors retain tenant and resource constraints.
pub mod resource_types {
    use super::{ResourceType, SUPPORTED_PROPERTIES, labels};
    pub const ACCEPTANCE: ResourceType =
        ResourceType::from_static(labels::ACCEPTANCE, SUPPORTED_PROPERTIES);
    pub const PRICE_BOOK: ResourceType =
        ResourceType::from_static(labels::PRICE_BOOK, SUPPORTED_PROPERTIES);
    pub const PRICE_BOOK_ENTRY: ResourceType =
        ResourceType::from_static(labels::PRICE_BOOK_ENTRY, SUPPORTED_PROPERTIES);
    pub const PRICE: ResourceType = ResourceType::from_static(labels::PRICE, SUPPORTED_PROPERTIES);
    pub const APPROVAL_UNIT: ResourceType =
        ResourceType::from_static(labels::APPROVAL_UNIT, SUPPORTED_PROPERTIES);
    pub const CONFIG: ResourceType =
        ResourceType::from_static(labels::CONFIG, SUPPORTED_PROPERTIES);
    pub const PLAN: ResourceType = ResourceType::from_static(labels::PLAN, SUPPORTED_PROPERTIES);
}

/// Supported tenant and resource constraints for future doors.
pub const SUPPORTED_PROPERTIES: &[&str] =
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

/// Operands of one refused authorization attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedAttempt {
    /// The principal that was refused.
    pub subject_principal_id: Uuid,
    /// The tenant the principal belongs to — not necessarily the resource's.
    pub subject_tenant_id: Uuid,
    /// The authz label the gate asked about.
    pub resource_type: String,
    /// The action asked for.
    pub action: String,
    /// The specific resource, when the gate named one.
    pub resource_id: Option<Uuid>,
    /// The resource's owning tenant, on the write paths that anchor to one.
    pub owner_tenant_id: Option<Uuid>,
    /// Why, in the words the caller may be told.
    pub reason: String,
}

/// Error from the catalog authz gate.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// The PDP explicitly denied access (or returned uncompilable constraints).
    #[error("permission denied: {}", .0.reason)]
    Denied(Box<DeniedAttempt>),
    /// The PDP was unreachable or its response could not be compiled.
    #[error("authz unavailable: {0}")]
    Unavailable(String),
}

/// The denial the PDP itself decided.
#[must_use]
pub fn pdp_denial(
    ctx: &SecurityContext,
    rt: &ResourceType,
    action: &str,
    resource_id: Option<Uuid>,
    owner_tenant_id: Option<Uuid>,
    reason: String,
) -> DeniedAttempt {
    DeniedAttempt {
        subject_principal_id: ctx.subject_id(),
        subject_tenant_id: ctx.subject_tenant_id(),
        resource_type: rt.name().to_owned(),
        action: action.to_owned(),
        resource_id,
        owner_tenant_id,
        reason,
    }
}

/// The denial this module decides itself: a write anchored to a tenant the
/// compiled scope does not contain.
///
/// Separate from [`pdp_denial`] because the PDP allowed this one — the degraded
/// flat-`In` decision does not re-validate `owner_tenant_id`, so the refusal is
/// this gate's own and a denial record must not attribute it to the PDP.
#[must_use]
pub fn cross_tenant_write_denial(
    subject_principal_id: Uuid,
    subject_tenant_id: Uuid,
    rt: &ResourceType,
    action: &str,
    resource_id: Option<Uuid>,
    target_tenant: Uuid,
) -> DeniedAttempt {
    DeniedAttempt {
        subject_principal_id,
        subject_tenant_id,
        resource_type: rt.name().to_owned(),
        action: action.to_owned(),
        resource_id,
        owner_tenant_id: Some(target_tenant),
        reason: format!(
            "subject not authorized to write resources owned by tenant {target_tenant}"
        ),
    }
}

/// Minimal, deterministic type-schema body for an authz label. Key order is
/// fixed by construction, so a re-registration is byte-identical — the registry
/// accepts identical duplicates and does not validate body richness.
fn authz_type_schema_json(gts_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
    })
}

/// Stub type-schemas for every authz label ([`labels::ALL`]). The platform RBAC
/// role-definition validator resolves a rule's `target_type` through the
/// types-registry, so registering these at gear init lets a custom catalog role
/// target any pricing authz label.
#[must_use]
pub fn authz_label_type_schemas() -> Vec<serde_json::Value> {
    labels::ALL
        .iter()
        .map(|label| {
            authz_type_schema_json(
                label,
                &format!("BSS Plan & Price Modeling authz label {label}"),
            )
        })
        .collect()
}

/// The tenant a write's row is addressed to — the `OWNER_TENANT_ID` hint.
///
/// A type rather than a bare `Option<Uuid>`, because it sits beside [`ResourceRef`]
/// in [`access_scope`]'s parameter list and two `Option<Uuid>` in adjacent positions
/// are interchangeable to the compiler wherever a call passes both. Swapped, the
/// membership assertion tests a resource id against the caller's tenants and denies a
/// write it should have allowed, while the PDP authorizes against a resource that does
/// not exist — whose answer is the deployed policy's, not this gear's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerTenant(pub Uuid);

/// The resource a request names — the `RESOURCE_ID` property.
///
/// [`OwnerTenant`]'s counterpart, for its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceRef(pub Uuid);

/// Shared PEP gate: asks the PDP whether `(resource_type, action)` is permitted
/// for `ctx`, returning the caller's compiled [`AccessScope`]. `resource_id`
/// pins a single-row op (`None` for collections).
///
/// `owner_tenant_id` is an optional `OWNER_TENANT_ID` resource-property hint
/// describing the *resource's* owning tenant:
/// - **Reads** pass `None` — the PDP derives the scope from the subject + role,
///   never from a caller-supplied tenant; the returned scope is the SQL filter.
/// - **Writes** pass `Some(target_tenant)` — the tenant the row is written to.
///   This is NOT self-validating at the PDP: the degraded flat-`In` decision
///   does not re-check `owner_tenant_id`, so this fn asserts `target_tenant` is
///   a member of the compiled scope and denies a cross-tenant target.
///
/// **Constraints are always required, and there is no parameter for it.** An
/// unconstrained *allow* on a read is every tenant's price book, and on a write it
/// leaves the target-membership assertion below with nothing to test — so both
/// authorizing paths need the same answer and neither has a reason to differ. Held
/// as a parameter it was a `false` away from fail-open at any one of the call
/// sites, spelled `true` at every one of them, and readable at none without a
/// `/* require_constraints */` comment beside it.
///
/// # Errors
///
/// [`AuthzError::Denied`] when the PDP denies or returns uncompilable
/// constraints; [`AuthzError::Unavailable`] when the PDP is unreachable.
pub async fn access_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    rt: &ResourceType,
    action: &str,
    owner_tenant_id: Option<OwnerTenant>,
    resource_id: Option<ResourceRef>,
) -> Result<AccessScope, AuthzError> {
    let owner_tenant_id = owner_tenant_id.map(|t| t.0);
    let resource_id = resource_id.map(|r| r.0);
    let mut request = AccessRequest::new().require_constraints(true);
    if let Some(tenant) = owner_tenant_id {
        request = request.resource_property(pep_properties::OWNER_TENANT_ID, tenant);
    }
    if let Some(rid) = resource_id {
        request = request.resource_property(pep_properties::RESOURCE_ID, rid);
    }
    let scope = enforcer
        .access_scope_with(ctx, rt, action, resource_id, &request)
        .await
        .map_err(|e| match e {
            authz_resolver_sdk::EnforcerError::Denied { ref deny_reason } => {
                let reason = deny_reason.as_ref().map_or_else(
                    || e.to_string(),
                    |dr| match &dr.details {
                        Some(details) => format!("{}: {details}", dr.error_code),
                        None => dr.error_code.clone(),
                    },
                );
                denied(pdp_denial(
                    ctx,
                    rt,
                    action,
                    resource_id,
                    owner_tenant_id,
                    reason,
                ))
            }
            authz_resolver_sdk::EnforcerError::CompileFailed(ref compile_err) => {
                // The compiler diagnostic names PDP predicates and properties — an
                // internal detail, not something the PDP told the caller. It goes
                // server-side only, with the same operands [`denied`] logs for a PDP
                // denial, so this line can be tied back to the 403 it explains. The
                // caller gets a stable machine token distinct from an actual PDP
                // denial, rather than the leaking diagnostic.
                tracing::warn!(
                    target: "pricing.authz.deny",
                    subject_principal_id = %ctx.subject_id(),
                    subject_tenant_id = %ctx.subject_tenant_id(),
                    resource_type = %rt.name(),
                    action,
                    resource_id = ?resource_id,
                    owner_tenant_id = ?owner_tenant_id,
                    error = %compile_err,
                    "authz constraint compilation failed"
                );
                AuthzError::Denied(Box::new(pdp_denial(
                    ctx,
                    rt,
                    action,
                    resource_id,
                    owner_tenant_id,
                    "constraint_compilation_failed".to_owned(),
                )))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                AuthzError::Unavailable(e.to_string())
            }
        })?;

    // Write paths anchor to a target tenant: the degraded flat-`In` PDP decision
    // does NOT re-validate `owner_tenant_id`, so assert the target is a member of
    // the compiled scope here — a target outside the caller's authorized tenants is
    // a cross-tenant write and is denied. Reads pass `owner_tenant_id = None` and
    // use the scope as the SQL filter, so this membership check is write-only, and
    // `Some(target)` is the whole of what selects it.
    if let Some(target) = owner_tenant_id
        && !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target)
    {
        return Err(denied(cross_tenant_write_denial(
            ctx.subject_id(),
            ctx.subject_tenant_id(),
            rt,
            action,
            resource_id,
            target,
        )));
    }
    Ok(scope)
}

/// A denial is an empty scope. An unreachable PDP stays unavailable (D-497).
pub async fn grant_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    action: &str,
) -> Result<AccessScope, AuthzError> {
    match access_scope(
        enforcer,
        ctx,
        &resource_types::APPROVAL_UNIT,
        action,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    {
        Ok(scope) => Ok(scope),
        Err(AuthzError::Denied(_)) => Ok(AccessScope::deny_all()),
        Err(error @ AuthzError::Unavailable(_)) => Err(error),
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

fn filter_holds(filter: &ScopeFilter, tenant: Uuid, id: Uuid) -> bool {
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

/// A denial with its operands in the log (PS-17): the 403 carries only the reason, and the
/// canonical middleware logs only the status and the path. Target `pricing.authz.deny`, as the
/// constraint compilation failure.
fn denied(attempt: DeniedAttempt) -> AuthzError {
    tracing::warn!(
        target: "pricing.authz.deny",
        subject_principal_id = %attempt.subject_principal_id,
        subject_tenant_id = %attempt.subject_tenant_id,
        resource_type = %attempt.resource_type,
        action = %attempt.action,
        resource_id = ?attempt.resource_id,
        owner_tenant_id = ?attempt.owner_tenant_id,
        reason = %attempt.reason,
        "authorization denied"
    );
    AuthzError::Denied(Box::new(attempt))
}

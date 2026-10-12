//! SKU, category, approval-unit and derived-usage-type authorization catalog and shared PEP gate.
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;
/// Concrete PDP-visible resource labels.
pub mod labels {
    use toolkit_gts::gts_id;
    pub const SKU: &str = gts_id!("cf.bss.products.sku.v1~");
    pub const CATEGORY: &str = gts_id!("cf.bss.products.category.v1~");
    pub const APPROVAL_UNIT: &str = gts_id!("cf.bss.products.approval_unit.v1~");
    /// A derived usage type (P-D-231): its writes ask `author` here; its reads ask `sku:read`
    /// (O-3).
    pub const DERIVED_USAGE_TYPE: &str = gts_id!("cf.bss.products.derived_usage_type.v1~");
    pub const ALL: &[&str] = &[SKU, CATEGORY, APPROVAL_UNIT, DERIVED_USAGE_TYPE];
}
/// Independent grants; reference is reserved for the gear-to-gear registry protocol.
pub mod actions {
    pub const READ: &str = "read";
    pub const AUTHOR: &str = "author";
    pub const SUBMIT: &str = "submit";
    pub const APPROVE: &str = "approve";
    pub const SETTINGS: &str = "settings";
    pub const REFERENCE: &str = "reference";
    pub const ALL: &[&str] = &[READ, AUTHOR, SUBMIT, APPROVE, SETTINGS, REFERENCE];
}
/// Every resource is scoped by its owning tenant and optional row identity.
pub const SUPPORTED_PROPERTIES: &[&str] =
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];
/// Resource descriptors bind PDP constraints to the supported properties.
pub mod resource_types {
    use super::{ResourceType, SUPPORTED_PROPERTIES, labels};
    pub const SKU: ResourceType = ResourceType::from_static(labels::SKU, SUPPORTED_PROPERTIES);
    pub const CATEGORY: ResourceType =
        ResourceType::from_static(labels::CATEGORY, SUPPORTED_PROPERTIES);
    pub const APPROVAL_UNIT: ResourceType =
        ResourceType::from_static(labels::APPROVAL_UNIT, SUPPORTED_PROPERTIES);
    pub const DERIVED_USAGE_TYPE: ResourceType =
        ResourceType::from_static(labels::DERIVED_USAGE_TYPE, SUPPORTED_PROPERTIES);
}
/// Error from the registry's PEP gate.
///
/// Deliberately **not** folded into [`crate::domain::error::DomainError`]:
/// that enum holds judgements a door reaches *after* it is authorized. A PDP
/// deny or an unreachable PDP happens *before* the domain is consulted at all,
/// so it answers with its own two-way split (403 vs 503), the same way the
/// ledger gear's `AuthzError` does, rather than borrowing a `DomainError` code
/// that would misdescribe why the door refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    /// The PDP explicitly denied access, or returned constraints this PEP
    /// could not compile (`authz_resolver_sdk::EnforcerError::Denied` and
    /// `CompileFailed` both land here — an uncompilable *allow* is refused
    /// exactly like an explicit deny, never treated as an unconstrained one).
    #[error("permission denied: {0}")]
    Denied(String),
    /// The PDP evaluation call itself failed — the resolver is unreachable or
    /// erroring, not exercising a business judgement
    /// (`authz_resolver_sdk::EnforcerError::EvaluationFailed`).
    #[error("authz unavailable: {0}")]
    Unavailable(String),
}

/// Shared PEP gate: asks the PDP whether `(resource_type, action)` is
/// permitted for `ctx`, returning the caller's compiled `AccessScope`.
///
/// `owner_tenant_id` is an optional `OWNER_TENANT_ID` resource-property hint
/// describing the *resource's* owning tenant:
/// - **Reads** pass `None` — the PDP derives the scope from the subject +
///   role, never from a caller-supplied tenant; the returned scope is the SQL
///   filter.
/// - **Writes** pass `Some(target_tenant)` — the tenant the row is written
///   to. This is NOT self-validating at the PDP: a degraded flat-`In`
///   decision does not re-check `owner_tenant_id`, so this fn asserts
///   `target_tenant` is a member of the compiled scope and denies a
///   cross-tenant target.
///
/// **Constraints are always required, and there is no parameter for it**
/// (RS-19, as pricing's gate): reads need them so the scope is a real SQL filter
/// and an unconstrained *allow* fail-closes instead of leaking every tenant, and
/// writes need them so the target-membership assertion has a constraint to test.
/// Held as a `bool` it was spelled `true` at every call site and was one `false`
/// away from skipping the cross-tenant check. No door names a single row, so the
/// `RESOURCE_ID` hint is not asked either.
///
/// Every denial is logged once here, target `bss_products.authz.deny`, with the
/// subject, its tenant, the resource type, the action, the target tenant and the
/// reason (RS-20): the 403 carries only the reason.
///
/// **The caller's tenant only (P-D-265).** Every request asks for
/// [`TenantMode::RootOnly`], so the PDP answers `EQ(owner_tenant_id, subject tenant)` — the
/// read's SQL filter — and never expands the subject's subtree. The registry serves one
/// tenant's SKUs and no other tenant's, so the subtree added no row the gear may show: it
/// only cost an `IN` list of every descendant tenant, deleted ones included, on every scope
/// check — thousands of bind parameters under a root tenant with thousands of descendants,
/// and a deny once the PDP's expansion cap is crossed. An `EQ` also stays decidable in memory,
/// which `scope_holds` needs for `caller_can_approve`.
///
/// # Errors
///
/// [`AuthzError::Denied`] when the PDP denies or returns uncompilable
/// constraints; [`AuthzError::Unavailable`] when the PDP is unreachable.
pub async fn access_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource: &ResourceType,
    action: &str,
    owner_tenant_id: Option<Uuid>,
) -> Result<AccessScope, AuthzError> {
    let mut request = AccessRequest::new()
        .require_constraints(true)
        .tenant_mode(TenantMode::RootOnly);
    if let Some(tenant) = owner_tenant_id {
        request = request.resource_property(pep_properties::OWNER_TENANT_ID, tenant);
    }

    let denial = |reason: String| {
        tracing::warn!(
            target: "bss_products.authz.deny",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            resource_type = resource.name(),
            action,
            owner_tenant_id = ?owner_tenant_id,
            reason = %reason,
            "bss-products: authorization denied"
        );
        AuthzError::Denied(reason)
    };
    let scope = enforcer
        .access_scope_with(ctx, resource, action, None, &request)
        .await
        .map_err(|e| match e {
            authz_resolver_sdk::EnforcerError::Denied { .. } => denial(e.to_string()),
            authz_resolver_sdk::EnforcerError::CompileFailed(ref compile_err) => {
                // The compiler's diagnostic names PDP predicates and properties: an internal
                // detail, not something the PDP told the caller. It stays in the log, and the
                // caller gets a stable token (RS-08, as pricing's gate).
                tracing::warn!(
                    target: "bss_products.authz.deny",
                    subject_id = %ctx.subject_id(),
                    resource_type = resource.name(),
                    action,
                    error = %compile_err,
                    "bss-products: authz constraint compilation failed"
                );
                denial(CONSTRAINT_COMPILATION_FAILED.to_owned())
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                AuthzError::Unavailable(e.to_string())
            }
        })?;

    // Write paths anchor to a target tenant: a degraded flat-`In` PDP decision
    // does NOT re-validate `owner_tenant_id`, so assert the target is a member of
    // the compiled scope here — a target outside the caller's authorized tenants
    // is a cross-tenant write and is denied. Reads pass `owner_tenant_id = None`
    // and use the scope as the SQL filter, so this membership check is
    // write-only, and `Some(target)` is the whole of what selects it.
    if let Some(target) = owner_tenant_id
        && !scope.contains_uuid(pep_properties::OWNER_TENANT_ID, target)
    {
        return Err(denial(format!(
            "subject not authorized to write resources owned by tenant {target}"
        )));
    }
    Ok(scope)
}

/// The reason a denial carries when the PDP's constraints did not compile (RS-08).
pub const CONSTRAINT_COMPILATION_FAILED: &str = "constraint_compilation_failed";

fn authz_type_schema_json(gts_id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "type": "object",
    })
}

/// Stub type-schemas for every authz label ([`labels::ALL`]). The platform
/// RBAC role-definition validator resolves a rule's `target_type` through the
/// types-registry, so registering these lets a custom catalog role target
/// this gear's authz labels.
///
/// **Registered from `Gear::init`** (P-D-204): a refused
/// registration fails the boot, as in the sibling pricing gear.
#[must_use]
pub fn authz_label_type_schemas() -> Vec<serde_json::Value> {
    labels::ALL
        .iter()
        .map(|label| authz_type_schema_json(label, &format!("BSS Products authz label {label}")))
        .collect()
}

#[cfg(test)]
#[path = "authz_tests.rs"]
mod authz_tests;

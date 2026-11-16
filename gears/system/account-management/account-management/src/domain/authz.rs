//! Shared PEP gate helper used by every AM domain service
//! (`TenantService`, `UserService`, `MetadataService`,
//! `ConversionService`).
//!
//! Before this helper landed each service redefined a near-identical
//! private `authorize(...)` that assembled an [`AccessRequest`]
//! (`OWNER_TENANT_ID`, optional `RESOURCE_ID`,
//! `require_constraints(true)`) and forwarded it through
//! [`PolicyEnforcer::access_scope_with`]. The metadata variant had
//! drifted to accept an extra `type_id` argument the others lacked;
//! any future change (extra default property, tracing fields,
//! fail-closed posture tweak) had to land four times and stay in sync
//! by eyeball. Routing every service through [`authz_scope`] collapses
//! those four surfaces into one definition site so the gate stays
//! coherent as new services are added.
//!
//! Per-service `authorize(...)` methods remain thin shims that
//! choose the resource type and supply the optional per-service
//! `AccessRequest` extension (e.g. `MetadataService` adds
//! `TYPE_ID`).

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Compile the caller's [`SecurityContext`] into an [`AccessScope`] for
/// `(resource_type, action, owner_tenant_id)`, optionally keyed on a
/// resource id, with caller-supplied extension of the underlying
/// [`AccessRequest`].
///
/// Always-on contract:
///
/// * `OWNER_TENANT_ID = owner_tenant_id` — consumed by ownership-style
///   policies.
/// * `RESOURCE_ID = resource_id` when `Some` — keyed against the
///   target row's id. Absent for surfaces with no single target
///   (e.g. `create` / `list_children` on `Tenant`).
/// * `require_constraints(true)` — a PDP returning
///   `decision: true, constraints: []` fails closed via
///   `CompileFailed → CrossTenantDenied` (HTTP 403) rather than
///   silently widening the read to `allow_all`.
///
/// `extend` runs LAST on the assembled request, so per-service
/// properties (the only current consumer is `MetadataService` which
/// attaches `TYPE_ID`) compose over the defaults instead of being
/// overwritten by them.
///
/// # Errors
///
/// * [`DomainError::CrossTenantDenied`] when the PDP denies the
///   decision or the returned constraint shape cannot be compiled
///   (`EnforcerError::AccessDenied` / `EnforcerError::CompileFailed`).
/// * [`DomainError::ServiceUnavailable`] when the PDP transport
///   fails — DESIGN §4.3 mandates fail-closed; AM does not provide a
///   local authorization fallback.
pub async fn authz_scope<F>(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource_type: &ResourceType,
    action: &str,
    owner_tenant_id: Uuid,
    resource_id: Option<Uuid>,
    extend: F,
) -> Result<AccessScope, DomainError>
where
    F: FnOnce(AccessRequest) -> AccessRequest,
{
    let mut request = AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, owner_tenant_id)
        .require_constraints(true);
    if let Some(rid) = resource_id {
        request = request.resource_property(pep_properties::RESOURCE_ID, rid);
    }
    let request = extend(request);
    let scope = enforcer
        .access_scope_with(ctx, resource_type, action, resource_id, &request)
        .await?;
    Ok(scope)
}

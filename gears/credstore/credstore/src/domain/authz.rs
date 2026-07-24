use authz_resolver_sdk::pep::{AccessRequest, EnforcerError, PolicyEnforcer, ResourceType};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};

use crate::domain::error::DomainError;

/// PDP resource type for a concrete secret type: the full derived GTS id
/// (design §5.4), e.g.
/// `gts.cf.core.credstore.secret.v1~cf.core.credstore.api_key.v1~`.
///
/// This is the **only** resource type credstore evaluates: every operation
/// authorizes against the secret's full concrete type (including `generic`),
/// so policies can target any type without a separate base-type gate. The id
/// comes from the per-operation types-registry resolution
/// ([`crate::domain::secret::type_resolver::ResolvedSecretType::gts_id`]),
/// so dynamically registered types are addressable without a release.
#[must_use]
pub fn secret_type_resource(gts_id: &str) -> ResourceType {
    ResourceType::new(gts_id.to_owned(), &[pep_properties::OWNER_TENANT_ID])
}

pub mod actions {
    pub const READ: &str = "read";
    pub const WRITE: &str = "write";
    pub const DELETE: &str = "delete";
}

/// Map a PEP enforcement failure to a domain error (fail-closed).
/// `Denied` / `CompileFailed` → `AccessDenied` (403); `EvaluationFailed` → `ServiceUnavailable` (503).
#[must_use]
pub fn map_enforcer_err(err: EnforcerError) -> DomainError {
    match err {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => {
            DomainError::AccessDenied {
                cause: Some(Box::new(err)),
            }
        }
        EnforcerError::EvaluationFailed(source) => DomainError::ServiceUnavailable {
            detail: "authorization evaluation failed".to_owned(),
            retry_after: None,
            cause: Some(Box::new(EnforcerError::EvaluationFailed(source))),
        },
    }
}

/// Returns the PDP `AccessScope` for `action` on `resource` for the
/// caller's tenant.
///
/// # Errors
///
/// Returns `DomainError::AccessDenied` if the PDP denies access or fails to compile constraints.
/// Returns `DomainError::ServiceUnavailable` if the PDP evaluation call fails.
pub async fn scope_for(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    resource: &ResourceType,
    action: &str,
) -> Result<AccessScope, DomainError> {
    let tenant = ctx.subject_tenant_id();
    let request = AccessRequest::new()
        .resource_property(pep_properties::OWNER_TENANT_ID, tenant)
        .require_constraints(true);
    enforcer
        .access_scope_with(ctx, resource, action, None, &request)
        .await
        .map_err(map_enforcer_err)
}

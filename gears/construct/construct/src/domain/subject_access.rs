//! Who may act on a subject's facts: the subject itself for its own
//! actions, and a caller the platform allows for the data protection
//! officer's export.

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::planner::manage::SUBJECT_FACTS_RESOURCE;
use super::profile_read::granted;
use super::subject_settings::SubjectKey;

/// The subject's key in the caller's tenant and the platform's scope for
/// `action` on its facts, when the caller is the subject and the platform
/// allows it.
///
/// # Errors
///
/// [`DomainError::Forbidden`] for another subject, before the policy service
/// is asked, and for a refusal or a scope that does not reach the subject.
pub(crate) async fn own(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    subject_id: Uuid,
    action: &str,
) -> Result<(SubjectKey, AccessScope), DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-compare
    if ctx.subject_id() != subject_id {
        return Err(DomainError::forbidden(format!(
            "subject {} of tenant {} may not {action} the facts of subject {subject_id}",
            ctx.subject_id(),
            ctx.subject_tenant_id()
        )));
    }
    // @cpt-end:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-compare
    // @cpt-begin:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-permission
    let key = SubjectKey {
        tenant_id: ctx.subject_tenant_id(),
        subject_id,
    };
    let scope = authorize(enforcer, ctx, key, action).await?;
    Ok((key, scope))
    // @cpt-end:cpt-cf-construct-algo-subject-control-owner-only:p1:inst-owner-permission
}

/// The platform's scope for `action` on the subject's facts, when it reaches
/// the subject in the tenant.
///
/// # Errors
///
/// [`DomainError::Forbidden`] for a refusal or a scope that does not reach
/// the subject.
pub(crate) async fn authorize(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    key: SubjectKey,
    action: &str,
) -> Result<AccessScope, DomainError> {
    let scope = enforcer
        .access_scope_with(
            ctx,
            &SUBJECT_FACTS_RESOURCE,
            action,
            Some(key.subject_id),
            &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id),
        )
        .await?;
    if granted(&scope, key).is_none() {
        return Err(DomainError::forbidden(format!(
            "the policy scope for {action} does not reach subject {} in tenant {}",
            key.subject_id, key.tenant_id
        )));
    }
    Ok(scope)
}

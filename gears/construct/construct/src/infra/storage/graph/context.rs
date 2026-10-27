//! The tenant check of a profile read or write, in a module of its own so
//! that nothing else can make a [`ContextSubject`].

use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::subject_settings::SubjectKey;

/// A subject in the security context's tenant. Only [`subject_in_context`]
/// makes one, so a profile read or write that takes it has passed the tenant
/// check.
#[derive(Debug, Clone, Copy)]
pub struct ContextSubject(SubjectKey);

impl ContextSubject {
    pub fn key(&self) -> &SubjectKey {
        &self.0
    }
}

/// `subject` when it is in the context's tenant. Graph storage reads and
/// writes in the security context's tenant, and the profile's keys name a
/// tenant too, so the two must be the same.
///
/// # Errors
///
/// [`DomainError::Internal`] when the subject's tenant is not the context's:
/// the caller built the context for another tenant, which is a defect.
pub fn subject_in_context(
    ctx: &SecurityContext,
    subject: SubjectKey,
) -> Result<ContextSubject, DomainError> {
    let context_tenant = ctx.subject_tenant_id();
    if context_tenant != subject.tenant_id {
        return Err(DomainError::internal(format!(
            "the profile's tenant {} is not the context's tenant {context_tenant}",
            subject.tenant_id
        )));
    }
    Ok(ContextSubject(subject))
}

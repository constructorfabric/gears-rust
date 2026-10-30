//! The security contexts OAGW acts under when no request carries one. Every
//! hand-built context in the gear lives here.
//!
//! The registry reconcile runs outside any request, as the platform's
//! default subject (`DEFAULT_SUBJECT_ID`) in the tenant each instance names,
//! trusted as the registry gives it. The policy engine is asked only when an
//! upstream binds to an ancestor's alias. With auth disabled every request
//! has the same identity, so neither the policy engine nor the credential
//! store can tell them apart (ADR-0018 *Tenant Scope*).

use toolkit_security::SecurityContext;
use toolkit_security::constants::{DEFAULT_SUBJECT_ID, DEFAULT_TENANT_ID};
use uuid::Uuid;

/// The registry reconcile's context for its reads and writes in `tenant_id`.
pub(crate) fn provisioning_ctx(tenant_id: Uuid) -> anyhow::Result<SecurityContext> {
    Ok(SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(DEFAULT_SUBJECT_ID)
        .build()?)
}

/// The context of the root-tenant lookup before a reconcile run: the tenant
/// resolver needs a subject, and the root tenant is what the run looks up.
pub(crate) fn bootstrap_ctx() -> anyhow::Result<SecurityContext> {
    Ok(SecurityContext::builder()
        .subject_id(DEFAULT_SUBJECT_ID)
        .subject_tenant_id(DEFAULT_TENANT_ID)
        .build()?)
}

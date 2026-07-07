//! Authentication-context extraction for the REST handlers.
//!
//! The provisioning endpoint requires an authenticated [`SecurityContext`];
//! requests without one MUST be refused with 401 — never silently treated as
//! an unauthenticated identity. The `billing-setup` PEP gate (scope / RBAC
//! action) is layered on top in P7.

use axum::extract::Extension;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::SecurityContext;

use crate::api::rest::error::unauthenticated;

/// Extract the [`SecurityContext`] from the request extensions, returning 401
/// when it is missing, carries the anonymous all-zero placeholder, or is
/// missing the positive `subject_type` marker that a real `AuthN` resolver
/// always populates.
///
/// # Errors
/// [`CanonicalError`] (401 unauthenticated) when no authenticated context is
/// present on the request.
pub(crate) fn require_authenticated(
    extension_ctx: Option<Extension<SecurityContext>>,
) -> Result<SecurityContext, CanonicalError> {
    let Some(Extension(ctx)) = extension_ctx else {
        return Err(unauthenticated());
    };
    if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
        return Err(unauthenticated());
    }
    if ctx.subject_type().is_none() {
        return Err(unauthenticated());
    }
    Ok(ctx)
}

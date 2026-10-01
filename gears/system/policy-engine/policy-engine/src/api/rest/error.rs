//! Canonical errors the transport detects before a request ever reaches
//! [`PolicyManagementClientV1`](policy_engine_sdk::management::PolicyManagementClientV1).
//!
//! A request with no security context, or an anonymous one, is
//! `unauthenticated` (401, reason
//! [`SECURITY_CONTEXT_REQUIRED`](policy_engine_sdk::error::reason::SECURITY_CONTEXT_REQUIRED)).
//! Every domain refusal is reported by the client itself and passes through
//! unchanged.

use axum::extract::Extension;
use policy_engine_sdk::error::reason::SECURITY_CONTEXT_REQUIRED;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// No security context, or an anonymous one: the request never reached
/// [`PolicyManagementClientV1`](policy_engine_sdk::management::PolicyManagementClientV1),
/// which cannot represent the absence of a caller.
#[must_use]
pub fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason(SECURITY_CONTEXT_REQUIRED)
        .create()
}

/// Requires a present, non-anonymous `SecurityContext` extension. Every
/// route here is `.authenticated()`; a request that reaches a handler
/// without one (in a router built without the platform's auth layer, as in a
/// unit test) is refused the same way a domain check downstream would refuse
/// it, rather than surfacing as axum's generic missing-extension `500`.
///
/// # Errors
///
/// [`unauthenticated`] when `ctx` is absent or the context is anonymous.
pub fn require_context(
    ctx: Option<Extension<SecurityContext>>,
) -> Result<SecurityContext, CanonicalError> {
    ctx.map(|Extension(ctx)| ctx)
        .filter(|ctx| !ctx.is_anonymous())
        .ok_or_else(unauthenticated)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn require_context_rejects_absent_and_anonymous() {
        assert_eq!(unauthenticated().status_code(), 401);
        assert!(require_context(None).is_err());
        let anonymous = SecurityContext::builder()
            .subject_id(uuid::Uuid::nil())
            .subject_tenant_id(uuid::Uuid::nil())
            .build()
            .expect("anonymous ctx");
        assert!(anonymous.is_anonymous());
        assert!(require_context(Some(Extension(anonymous))).is_err());
        let real = SecurityContext::builder()
            .subject_id(uuid::Uuid::from_u128(1))
            .subject_tenant_id(uuid::Uuid::from_u128(2))
            .build()
            .expect("ctx");
        assert!(require_context(Some(Extension(real))).is_ok());
    }
}

//! REST-level error mapping: the authz gate and missing-authentication cases.
//!
//! Domain rejections reach the wire through the single
//! `From<DomainError> for CanonicalError` ladder
//! ([`crate::infra::error_mapping`]), never from here — this module handles only
//! what happens before a handler reaches the domain at all.

use toolkit::api::canonical_prelude::{CanonicalError, resource_error};

use crate::authz::AuthzError;

/// Stamps `context.resource_type` on the canonical error.
#[resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
struct PricingResourceError;

/// Map an [`AuthzError`] from the PEP gate to a [`CanonicalError`].
///
/// `Denied` becomes a 403 carrying the deny reason. `Unavailable` becomes a
/// fail-closed 503 whose diagnostic stays server-side: a PDP outage must not
/// degrade into an allow, and it must not tell the caller why the policy engine
/// is unhappy either.
pub(crate) fn authz_error_to_canonical(err: AuthzError) -> CanonicalError {
    match err {
        AuthzError::Denied(attempt) => {
            // `inst-rb-audit` / `dod-rbac`, both `p1`: a denied attempt leaves a
            // trace. It left none — this arm returned in silence while the
            // `Unavailable` arm below logged, which is the tell. Emitted here and
            // not at the gate because this is the single funnel every one of the
            // layer's 403s passes through, so one site covers every route and a
            // new route cannot forget it.
            //
            // `warn`, not `info`: a refusal is the security event the operator
            // asked for. The full operand set goes to the log; the caller is told
            // only `reason`, which is what the PDP already said out loud.
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
            PricingResourceError::permission_denied()
                .with_reason(attempt.reason)
                .create()
        }
        AuthzError::Unavailable(detail) => {
            tracing::error!(detail, "authorization service unavailable");
            CanonicalError::service_unavailable().create()
        }
    }
}

/// Build a 401 for a request without an authenticated `SecurityContext` —
/// distinct from a permission denial (403), which means the caller is known and
/// not allowed.
pub(crate) fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason("AUTHENTICATION_REQUIRED")
        .create()
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;

//! `DomainError` → [`CanonicalError`] boundary mapping for the token-issuer
//! REST layer.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// Resource marker
// ---------------------------------------------------------------------------

#[resource_error(gts_id!("cf.core.token_issuer.token.v1~"))]
pub(crate) struct TokenResource;

// ---------------------------------------------------------------------------
// DomainError → CanonicalError
// ---------------------------------------------------------------------------

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::InvalidRequest { detail } => TokenResource::invalid_argument()
                .with_field_violation("request", detail, "INVALID_REQUEST")
                .create(),
            // Signing/NotReady are transient: the caller can retry.
            DomainError::Signing { detail } => CanonicalError::service_unavailable()
                .with_detail(detail)
                .create(),
            DomainError::NotReady => CanonicalError::service_unavailable()
                .with_detail("token issuer not ready")
                .create(),
            // Cap-token provenance / expiry failures → 401 (re-mint Gate 1).
            // The detail is not surfaced verbatim — it names the failed check.
            DomainError::CapInvalid { .. } => CanonicalError::unauthenticated()
                .with_reason("capability token invalid")
                .create(),
            // Peer / adapter authorization failures → 403 (re-mint Gates 1 & 2).
            // All collapse to a single opaque permission-denied (no probing).
            DomainError::PeerUnverified
            | DomainError::PeerUnknown
            | DomainError::PeerMismatch
            | DomainError::AdapterInactive
            | DomainError::OboNotGranted
            | DomainError::LoopGuard => TokenResource::permission_denied()
                .with_reason("OBO re-mint not permitted")
                .create(),
            // OBO disabled by config → 404 (the surface is simply absent).
            DomainError::OboDisabled => TokenResource::not_found("OBO issuance is not enabled")
                .with_resource("obo")
                .create(),
            DomainError::Internal { diagnostic } => CanonicalError::internal(diagnostic).create(),
        }
    }
}

#[cfg(test)]
mod tests {
    use toolkit_canonical_errors::CanonicalError;

    use crate::domain::error::DomainError;

    fn status_of(err: DomainError) -> u16 {
        CanonicalError::from(err).status_code()
    }

    #[test]
    fn maps_each_variant_to_expected_status() {
        assert_eq!(
            status_of(DomainError::InvalidRequest {
                detail: "bad".to_owned()
            }),
            400
        );
        assert_eq!(
            status_of(DomainError::Signing {
                detail: "down".to_owned()
            }),
            503
        );
        assert_eq!(status_of(DomainError::NotReady), 503);
        assert_eq!(status_of(DomainError::internal("boom")), 500);
    }

    #[test]
    fn maps_obo_remint_variants_to_expected_status() {
        // Gate 1 provenance / expiry → 401.
        assert_eq!(status_of(DomainError::cap_invalid("sig")), 401);
        // Peer / adapter / loop-guard authorization failures → 403.
        for err in [
            DomainError::PeerUnverified,
            DomainError::PeerUnknown,
            DomainError::PeerMismatch,
            DomainError::AdapterInactive,
            DomainError::OboNotGranted,
            DomainError::LoopGuard,
        ] {
            assert_eq!(status_of(err), 403);
        }
        // Disabled feature → 404.
        assert_eq!(status_of(DomainError::OboDisabled), 404);
    }
}

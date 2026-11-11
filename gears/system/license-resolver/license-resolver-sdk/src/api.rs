//! Public API contract for the license resolver.
//!
//! `LicenseResolverApi` is a transport-agnostic `#[toolkit::contract]`: the
//! gear registers a local impl in `ClientHub` for in-process consumers, and the
//! [`rest`](crate::rest) projection generates a REST client for out-of-process
//! ones. Consumers do not branch on the binding.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::models::{IsLicensedRequest, LicenseDecision, SelfLicenseCheckRequest};

/// Public API contract for the license resolver gateway.
#[toolkit::contract(gear = "license-resolver", version = "v1")]
pub trait LicenseResolverApi: Send + Sync {
    /// Point-in-time check of whether a resource is licensed to a subject.
    ///
    /// A platform-plane method: `ctx` marks the plane and carries no identity.
    /// Over REST the transport authenticates the calling workload; the tenant
    /// scope of the check rides in `request.context`, which the caller derives
    /// from its own `SecurityContext`. Callers without a
    /// [`PlatformSecurityContext`] pass [`PlatformSecurityContext::outbound_marker`].
    ///
    /// A principal subject is first mapped to the Subject contract type the
    /// resolver's configuration names for its principal type; a principal type
    /// without a mapping is `LicenseDecision { granted: false }`. The resolver
    /// then validates the request against the registered contracts and
    /// delegates to the selected backend plugin. A not-granted answer is
    /// `LicenseDecision { granted: false }`, not an error.
    ///
    /// # Errors
    ///
    /// The [`LicenseResolverError`](crate::LicenseResolverError) cases, mapped
    /// to `CanonicalError` as listed in [`crate::error`]. On any error the
    /// caller fails closed.
    #[idempotency(SafeRead)]
    async fn is_licensed(
        &self,
        ctx: PlatformSecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;

    /// [`is_licensed`](Self::is_licensed) for the principal of `ctx`, scoped to
    /// its tenant: the subject is `ctx.subject_type()` / `ctx.subject_id()`, the
    /// tenant is `ctx.subject_tenant_id()`.
    ///
    /// # Errors
    ///
    /// As [`is_licensed`](Self::is_licensed).
    #[idempotency(SafeRead)]
    async fn is_self_licensed(
        &self,
        ctx: SecurityContext,
        request: SelfLicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

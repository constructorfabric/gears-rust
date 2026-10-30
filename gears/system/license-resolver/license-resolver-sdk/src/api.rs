//! Public API contract for the license resolver.
//!
//! `LicenseResolverApi` is a transport-agnostic `#[toolkit::contract]`: the
//! gear registers a local impl in `ClientHub` for in-process consumers, and the
//! [`rest`](crate::rest) projection generates a REST client for out-of-process
//! ones. Consumers do not branch on the binding.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::models::{LicenseCheckRequest, LicenseDecision};

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
    /// The resolver validates the request against the registered contracts,
    /// then delegates to the selected backend plugin. A not-granted answer is
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
        request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

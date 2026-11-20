//! Public API contracts for the license resolver, one per plane, as in
//! types-registry.
//!
//! Both are transport-agnostic `#[toolkit::contract]`s: the gear registers a
//! local impl of each in `ClientHub` for in-process consumers, and the
//! [`rest`](crate::rest) projections generate REST clients for out-of-process
//! ones. Consumers do not branch on the binding.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::models::{IsLicensedRequest, LicenseDecision, PlatformIsLicensedRequest};

/// The platform plane: a trusted platform service names the subject and the
/// tenant of the check.
#[toolkit::contract(gear = "license-resolver", version = "v1")]
pub trait PlatformLicenseResolverApi: Send + Sync {
    /// Point-in-time check of whether a resource is licensed to a subject.
    ///
    /// `ctx` marks the plane and carries no identity. Over REST the transport
    /// authenticates the calling workload. A module that checks the user of its
    /// request passes the user's principal and tenant, read from its own
    /// `SecurityContext`, as request data. Callers without a
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
        request: PlatformIsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

/// The tenant plane: the check for the caller itself.
#[toolkit::contract(gear = "license-resolver", version = "v1")]
pub trait LicenseResolverApi: Send + Sync {
    /// [`PlatformLicenseResolverApi::is_licensed`] for the principal of `ctx`,
    /// scoped to its tenant: the subject is `ctx.subject_type()` /
    /// `ctx.subject_id()`, the tenant is `ctx.subject_tenant_id()`. The request
    /// carries the resource and the operation context.
    ///
    /// # Errors
    ///
    /// As [`PlatformLicenseResolverApi::is_licensed`].
    #[idempotency(SafeRead)]
    async fn is_licensed(
        &self,
        ctx: SecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

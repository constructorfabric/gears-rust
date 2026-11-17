//! REST projections of the two public contracts.
//!
//! Feature `rest-client` emits the `*RestClient` types and their
//! directory-resolving wrappers; `rest-server` emits the `register_*_rest_routes`
//! functions the gear hosts.
//!
//! The platform route is internal: the generated client attaches the process's
//! service credential, `internal_auth_middleware` turns it into a
//! `PlatformSecurityContext`; external clients hold no service credential, so
//! they cannot call it. The tenant route takes the caller's bearer token and is exposed: it
//! answers only about the authenticated principal.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::api::{LicenseResolverApi, PlatformLicenseResolverApi};
use crate::models::{IsLicensedRequest, LicenseDecision, PlatformIsLicensedRequest};

/// HTTP projection of [`PlatformLicenseResolverApi`].
#[toolkit::rest_contract(base_path = "/license-resolver/v1/platform")]
pub trait PlatformLicenseResolverApiRest: PlatformLicenseResolverApi {
    /// `POST /license-resolver/v1/platform/is-licensed`.
    #[post("/is-licensed")]
    async fn is_licensed(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformIsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

/// HTTP projection of [`LicenseResolverApi`].
#[toolkit::rest_contract(base_path = "/license-resolver/v1")]
pub trait LicenseResolverApiRest: LicenseResolverApi {
    /// `POST /license-resolver/v1/is-licensed`.
    #[post("/is-licensed")]
    #[exposed]
    async fn is_licensed(
        &self,
        ctx: SecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

//! REST projection of [`LicenseResolverApi`].
//!
//! Feature `rest-client` emits `LicenseResolverApiRestClient` and its
//! directory-resolving wrapper; `rest-server` emits
//! `register_license_resolver_api_rest_routes` for the gear to host.
//!
//! `is-licensed` is platform-only: the generated client attaches the process's
//! service credential, `internal_auth_middleware` turns it into a
//! `PlatformSecurityContext`, and the edge strips that credential from external
//! requests. `is-self-licensed` takes the caller's bearer token and is
//! public: it answers only about the authenticated principal.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::api::LicenseResolverApi;
use crate::models::{IsLicensedRequest, LicenseDecision, SelfLicenseCheckRequest};

/// HTTP projection of [`LicenseResolverApi`].
#[toolkit::rest_contract(base_path = "/license-resolver/v1")]
pub trait LicenseResolverApiRest: LicenseResolverApi {
    /// `POST /license-resolver/v1/is-licensed`.
    #[post("/is-licensed")]
    async fn is_licensed(
        &self,
        ctx: PlatformSecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError>;

    /// `POST /license-resolver/v1/is-self-licensed`.
    #[post("/is-self-licensed")]
    #[exposed]
    async fn is_self_licensed(
        &self,
        ctx: SecurityContext,
        request: SelfLicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

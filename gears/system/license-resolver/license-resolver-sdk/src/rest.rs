//! REST projection of [`LicenseResolverApi`].
//!
//! Feature `rest-client` emits `LicenseResolverApiRestClient` and its
//! directory-resolving wrapper; `rest-server` emits
//! `register_license_resolver_api_rest_routes` for the gear to host.
//!
//! The route is internal: the generated client attaches the process's service
//! credential, `internal_auth_middleware` turns it into a
//! `PlatformSecurityContext`, and the edge api-gateway does not expose it.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::api::LicenseResolverApi;
use crate::models::{LicenseCheckRequest, LicenseDecision};

/// HTTP projection of [`LicenseResolverApi`].
#[toolkit::rest_contract(base_path = "/license-resolver/v1")]
pub trait LicenseResolverApiRest: LicenseResolverApi {
    /// `POST /license-resolver/v1/is-licensed`.
    #[post("/is-licensed")]
    async fn is_licensed(
        &self,
        ctx: PlatformSecurityContext,
        request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError>;
}

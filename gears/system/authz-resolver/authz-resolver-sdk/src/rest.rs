//! REST projection of [`AuthZResolverApi`].
//!
//! Carries the HTTP method/path annotations consumed by
//! `#[toolkit::rest_contract]`. When the `rest-client` feature is enabled the
//! macro also emits `AuthZResolverApiRestClient` (and its directory-resolving
//! wrapper `AuthZResolverApiRestResolvingClient`) that implement
//! [`AuthZResolverApi`] over HTTP; when `rest-server` is enabled it emits
//! `register_auth_z_resolver_api_rest_routes` for the gear to host.
//!
//! The `evaluate` route is **internal** — authenticated on the **platform
//! plane** (`cpt-cf-adr-two-plane-auth`): the generated client attaches the
//! process's internal service-identity credential (`X-ToolKit-Internal-Token`)
//! below the contract layer, and `internal_auth_middleware` validates it into a
//! `PlatformSecurityContext` before the handler runs. The route is deliberately
//! not marked public, so the edge api-gateway does not expose it to external
//! clients. Only in-cluster PEPs reach it directly via directory resolution.
//!
//! Because the transport authenticates the *calling workload* (not an end
//! user), the PDP trusts the authorization `subject` supplied in `req.subject`
//! — the trust model per `DESIGN.md` (subject identity originates at AuthN and
//! flows AuthN → PEP → PDP). The `ctx` argument is a compile-time plane marker
//! only; it carries no identity and is never used for an authorization
//! decision.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::api::AuthZResolverApi;
use crate::models::{EvaluationRequest, EvaluationResponse};

/// HTTP projection of [`AuthZResolverApi`].
#[toolkit::rest_contract(base_path = "/authz-resolver/v1")]
pub trait AuthZResolverApiRest: AuthZResolverApi {
    /// `POST /authz-resolver/v1/evaluate` — evaluate an `AuthZEN` request.
    #[post("/evaluate")]
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError>;
}

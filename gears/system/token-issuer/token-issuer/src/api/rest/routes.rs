//! Public REST route registration for the token-issuer.
//!
//! All routes are `public()` (no auth, no license) — JWKS and discovery are
//! published so verifiers can validate minted tokens. The capability and grant
//! issuer surfaces are always registered; the OBO issuer routes + the re-mint
//! endpoint are registered only behind the `svc.obo_enabled()` guard
//! (DESIGN.md § 3.3).
//!
//! The re-mint endpoint is also `.public()` by design: its auth is the
//! presented capability token + the mTLS peer identity, both verified
//! in-handler / in-domain (there is no user/KC bearer). See
//! [`handlers::remint_obo`].

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::handlers;
use crate::api::rest::dto::{RemintRequest, RemintResponse};
use crate::domain::service::Service;

const TAG: &str = "Token Issuer";

/// Registers the public JWKS + discovery routes.
///
/// The issuer paths (`/issuers/cap/jwks.json`,
/// `/issuers/cap/.well-known/openid-configuration`) are intentionally
/// unversioned identifier surfaces (DESIGN.md § 3.3) — like `/livez`/`/readyz`,
/// they are stable public endpoints addressed by external verifiers, not
/// versioned APIs — so we suppress `de0801_api_endpoint_version`.
#[allow(unknown_lints)]
#[allow(
    de0801_api_endpoint_version,
    reason = "issuer paths are intentionally unversioned identifier surfaces per DESIGN.md § 3.3"
)]
pub fn register_routes(router: Router, openapi: &dyn OpenApiRegistry, svc: Arc<Service>) -> Router {
    let router = OperationBuilder::get("/issuers/cap/jwks.json")
        .operation_id("token_issuer.cap_jwks")
        .summary("Capability-token JWKS")
        .description("Public JSON Web Key Set used to verify capability tokens.")
        .tag(TAG)
        .public()
        .handler(handlers::cap_jwks)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "JWKS")
        .register(router, openapi);

    let router = OperationBuilder::get("/issuers/cap/.well-known/openid-configuration")
        .operation_id("token_issuer.cap_discovery")
        .summary("Capability issuer discovery")
        .description("OIDC-style discovery document for the capability issuer.")
        .tag(TAG)
        .public()
        .handler(handlers::cap_discovery)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "OIDC discovery")
        .register(router, openapi);

    // Grant issuer surface (`/issuers/grant/...` JWKS + discovery). Always
    // registered — the `grants` gear depends on the grant class from slice 1, and
    // adapters fetch these keys offline to verify presented grants. Anonymous /
    // public, like the cap pair.
    let router = OperationBuilder::get("/issuers/grant/jwks.json")
        .operation_id("token_issuer.grant_jwks")
        .summary("Grant-token JWKS")
        .description("Public JSON Web Key Set used to verify data-plane grant tokens.")
        .tag(TAG)
        .public()
        .handler(handlers::grant_jwks)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "JWKS")
        .register(router, openapi);

    let router = OperationBuilder::get("/issuers/grant/.well-known/openid-configuration")
        .operation_id("token_issuer.grant_discovery")
        .summary("Grant issuer discovery")
        .description("OIDC-style discovery document for the grant issuer.")
        .tag(TAG)
        .public()
        .handler(handlers::grant_discovery)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "OIDC discovery")
        .register(router, openapi);

    // OBO issuer surface (`/issuers/obo/...` JWKS + discovery, and the re-mint
    // endpoint) — registered only when OBO is enabled. With the default
    // `obo.enabled = false`, none of these are registered.
    let router = if svc.obo_enabled() {
        register_obo_routes(router, openapi)
    } else {
        router
    };

    router.layer(axum::Extension(svc))
}

/// Registers the OBO issuer JWKS + discovery and the re-mint endpoint. Called
/// only when `obo.enabled`. All three are `.public()` — the cap token + mTLS
/// peer are the auth for the re-mint endpoint (verified in-handler), and the
/// JWKS/discovery are public identifier surfaces like the cap pair.
#[allow(unknown_lints)]
#[allow(
    de0801_api_endpoint_version,
    reason = "issuer paths are intentionally unversioned identifier surfaces per DESIGN.md § 3.3"
)]
fn register_obo_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/issuers/obo/jwks.json")
        .operation_id("token_issuer.obo_jwks")
        .summary("OBO-token JWKS")
        .description("Public JSON Web Key Set used to verify OBO tokens.")
        .tag(TAG)
        .public()
        .handler(handlers::obo_jwks)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "JWKS")
        .register(router, openapi);

    let router = OperationBuilder::get("/issuers/obo/.well-known/openid-configuration")
        .operation_id("token_issuer.obo_discovery")
        .summary("OBO issuer discovery")
        .description("OIDC-style discovery document for the OBO issuer.")
        .tag(TAG)
        .public()
        .handler(handlers::obo_discovery)
        .json_response_with_schema::<serde_json::Value>(openapi, StatusCode::OK, "OIDC discovery")
        .register(router, openapi);

    // Re-mint endpoint. `.public()`: the auth is the presented capability token
    // + the mTLS peer identity, both verified in-handler / in-domain (`.public()`
    // also implies no license). mTLS exposure is external (DESIGN.md § 4.1);
    // until then the peer cert is absent and the resolver fail-closes (403).
    OperationBuilder::post("/internal/v1/issuers/obo/tokens")
        .operation_id("token_issuer.remint_obo")
        .summary("Re-mint a capability token into a down-scoped OBO token")
        .description(
            "Verifies the presented capability token (Authorization: Bearer), binds it to the \
             mTLS peer, down-scopes against the adapter's registry allowlist, and returns an \
             idempotent OBO token. Public: the cap token + mTLS peer are the auth.",
        )
        .tag(TAG)
        .public()
        .json_request::<RemintRequest>(openapi, "Optional requested scope subset")
        .handler(handlers::remint_obo)
        .json_response_with_schema::<RemintResponse>(openapi, StatusCode::OK, "Minted OBO token")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;

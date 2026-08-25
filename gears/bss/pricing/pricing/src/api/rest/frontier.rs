//! `GET /bss-pricing/v1/catalog-version/frontier` — the pin-eligibility
//! watermark (D-136; `design/01-foundation.md` §3.3 and §4.4).
//!
//! This is the entry point of the published read-model contract: a consumer
//! (Tariffs, Rating, Subscriptions, Billing, holding `plan x read` as a service
//! identity) reads the frontier **once**, pins its `catalog_version` for the
//! whole of a resolution or rating run, and resolves everything else at that
//! pin. Pin-eligibility is a version-level, prefix-closed predicate (D-101 +
//! D-114) no consumer can evaluate for itself, and D-136 makes it a stored
//! per-tenant watermark rather than a read-time recomputation, so this route is
//! a single point lookup, not a scan.
//!
//! The route is a read: it gates on `plan x read` with
//! `owner_tenant_id = None`, so the PDP derives the caller's scope and the
//! compiled `AccessScope` becomes the SQL-level tenant filter. A frontier
//! belonging to a tenant outside that scope is invisible rather than forbidden,
//! which is what keeps the surface from leaking whose catalog exists.

use std::sync::Arc;

use axum::extract::Extension;
use axum::{Json, Router, http::StatusCode};
use chrono::{DateTime, Utc};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_security::SecurityContext;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::error::authz_error_to_canonical;
use crate::infra::storage::repo::PinFrontierRepo;

/// `OpenAPI` tag applied to the catalog-version operations (DE0205 requires a
/// tag and a summary on every registered operation).
const TAG: &str = "BSS Pricing Catalog Version";

/// The pin-eligibility frontier read (D-136).
///
/// The literal is repeated in the `OperationBuilder` call below because DE0801
/// validates a **literal** argument and silently passes a `const` one, so the
/// route-shape rule only binds where the literal is; the two spellings are
/// pinned together by `tests/module_test.rs`'s route census. It exists as a
/// `const` for the reason its siblings do: a route census that spelled one of
/// its paths as a string literal is a census one rename can walk away from.
pub const FRONTIER: &str = "/bss-pricing/v1/catalog-version/frontier";

/// Shared per-request state for the catalog-version routes. Built once in
/// `init()` and shared via `Extension<Arc<ApiState>>`.
#[derive(Clone)]
pub struct ApiState {
    /// The materialized pin-eligibility frontier (`pricing_pin_frontier`).
    pub pin_frontier: PinFrontierRepo,
}

/// The tenant's pin-eligibility frontier.
///
/// **Why this is a 200 with an explicit empty shape and not a 404.**
///
/// A tenant that has never completed a publish has no pin-eligible version.
/// That is a *state of the watermark*, not the absence of a resource: the
/// frontier is a per-tenant singleton that exists for every tenant from the
/// moment the tenant does, and `pin_eligible = false` is one of its two legal
/// readings. Three things follow, and each of them argues against 404.
///
/// - A consumer must be able to distinguish "no publish has ever completed"
///   from "the frontier stands at version 0". Both are answered here without
///   ambiguity: `pin_eligible` is the discriminator, and `catalog_version` is
///   `null` in the first case and `0` in the second. A consumer never has to
///   infer the state from a missing field.
/// - This gear's 404 deliberately answers identically for "absent" and "outside
///   your scope" (`DomainError::NotFound`, so the surface leaks no existence).
///   Serving the empty frontier as a 404 would therefore make it
///   indistinguishable from a scope denial — exactly the discrimination the
///   contract requires, destroyed by the status choice.
/// - Consumers poll this endpoint at tenant onboarding, waiting for the first
///   publish to land. A 404 in that loop is normally an operational error and
///   attracts retry, backoff and alerting machinery; here it would be the
///   expected steady state of a healthy, freshly provisioned tenant.
///
/// The in-process contract (`bss_pricing_sdk::PricingCatalogClientV1`) **agrees**.
/// It does not return a `FailedPrecondition` here and its signature does not
/// promise a `PinFrontier`: it is
/// `-> Result<Option<PinFrontier>, CanonicalError>`, its `# Errors` section lists
/// only `PermissionDenied` and `Unavailable`, and its own doc says in as many
/// words that `None` is *"a **state, not an error** … deliberately **not folded
/// into an error**"*. So the wire's `200` with `pin_eligible: false` and the
/// trait's `Ok(None)` are one reading, not two.
///
/// Worth the correction because the trait has **no implementor**: whoever writes
/// the first one reads this file for the contract, and folding "nothing to pin
/// yet" into a refusal would meet every consumer as an outage at tenant
/// onboarding — the exact confusion both docs otherwise spend paragraphs
/// preventing, and one no test could catch while there is nothing to test.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PinFrontierView {
    /// `true` when the tenant has a version a consumer may pin. `false` means
    /// no publish has ever completed for this tenant — never that the frontier
    /// is stuck or unknown.
    pub pin_eligible: bool,
    /// The newest pin-eligible `CatalogVersion`, or `null` when
    /// `pin_eligible` is `false`. Advanced only forward, and only by the
    /// projector inside the transaction that completes the frontier's next
    /// version in order.
    pub catalog_version: Option<u64>,
    /// UTC instant the frontier last advanced, or `null` when it never has.
    /// The referent of the 5s pin-lag rule and of the
    /// `pricing.readmodel.pin_eligibility_overdue` alarm.
    pub advanced_at: Option<DateTime<Utc>>,
}

impl PinFrontierView {
    /// The reading for a tenant with nothing to pin.
    fn none_yet() -> Self {
        Self {
            pin_eligible: false,
            catalog_version: None,
            advanced_at: None,
        }
    }
}

impl From<bss_pricing_sdk::PinFrontier> for PinFrontierView {
    fn from(frontier: bss_pricing_sdk::PinFrontier) -> Self {
        Self {
            pin_eligible: true,
            catalog_version: Some(frontier.catalog_version.get()),
            advanced_at: Some(frontier.advanced_at),
        }
    }
}

/// Build the Axum router for the catalog-version surface and register its
/// operations with the supplied `OpenAPI` registry.
///
/// The declared error responses are the ones this path can actually produce:
/// 401 (no authenticated context), 403 (the PDP denies), 503 (the PDP is
/// unreachable, or the read model is unavailable — both fail closed) and 500.
/// No 400: the operation takes no body and no parameters, so there is no
/// request a caller can malform. No 422 either — the design set's 422s are
/// architectural and reach the wire as 400s carrying their code, so no path in
/// this gear can produce one (see `infra::error_mapping`).
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/bss-pricing/v1/catalog-version/frontier")
        .operation_id("bss_pricing.get_catalog_version_frontier")
        .summary("Read the tenant pin-eligibility frontier")
        .description(
            "Returns the caller tenant's current pin-eligibility frontier (D-136): the newest \
             `CatalogVersion` a consumer may pin, and the instant it last advanced. A consumer \
             pins this value once and resolves the whole of a resolution or rating run against \
             it; pin-eligibility is a version-level, prefix-closed predicate (D-101 / D-114) \
             that no consumer can evaluate for itself. A tenant whose first publish has not \
             completed is answered `200` with `pin_eligible: false` and a null version - it has \
             nothing it may pin, which is distinct both from a missing resource and from a \
             frontier standing at version 0. Gates on `plan` x `read`; tenant-scoped at the SQL \
             level, so a frontier outside the caller's scope reads as absent rather than \
             forbidden.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .handler(get_frontier)
        .json_response_with_schema::<PinFrontierView>(
            openapi,
            StatusCode::OK,
            "The tenant's pin-eligibility frontier, or the explicit `pin_eligible: false` \
             reading when no publish has completed.",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);

    router.layer(Extension(state))
}

/// `GET /catalog-version/frontier`: read the caller tenant's frontier.
///
/// `owner_tenant_id` is `None` — this is a read, so the PDP derives the scope
/// from the subject and its roles rather than trusting a caller-supplied tenant,
/// and the compiled scope is the SQL filter. `require_constraints = true` so an
/// unconstrained allow fail-closes instead of exposing every tenant's frontier.
async fn get_frontier(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
) -> Result<Json<PinFrontierView>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::PLAN,
        crate::authz::actions::READ,
        /* owner_tenant_id */ None,
        /* resource_id */ None,
    )
    .await
    .map_err(authz_error_to_canonical)?;

    let frontier = state
        .pin_frontier
        .read(&scope, ctx.subject_tenant_id())
        .await
        // Through the gear's SINGLE authoritative ladder, not a mapping
        // invented here: `infra::storage::repo_failure` already decides what a
        // storage failure means — it is the only conversion, there is no
        // `From<RepoError>` impl to reach for — and forking it per handler is how
        // the two surfaces start disagreeing. What matters on this path is that a
        // failure is never substituted by an empty frontier — a consumer that
        // reads "nothing to pin" stops, and one that reads a wrong version
        // resolves an entire run against it.
        .map_err(|e| CanonicalError::from(crate::infra::storage::repo_failure(&e)))?;

    Ok(Json(frontier.map_or_else(
        PinFrontierView::none_yet,
        PinFrontierView::from,
    )))
}

#[cfg(test)]
#[path = "frontier_tests.rs"]
mod frontier_tests;

//! The route of the activation read.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::activation_dto::ActivationDto;
use crate::api::rest::activation_handlers as handlers;
use crate::gear::{ConcreteRecorder, ConcreteResolver};

const TAG: &str = "settings-values";

/// Register the activation read.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    resolver: Arc<ConcreteResolver>,
    recorder: Arc<ConcreteRecorder>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::get("/settings-service/v1/settings/{key}/activation")
        .operation_id("settings_service.get_setting_activation")
        .summary("Read a setting's activation at a tenant")
        .description(
            "Which consumers of a setting have taken its current value up at one tenant: one \
             item per subscriber, ordered by subscriber, with its `status`, the `change_ref` \
             of the change it answers, `reported_at` once the state is settled and `detail` \
             on a `failed` state. The answer carries no value, expected or applied, and no \
             hash. `status` is `awaiting`, `succeeded` or `failed`; treat it as an open set \
             and read an unknown status as not known to have activated. An `awaiting` item \
             stays listed until a report or a later change, with no deadline. `tenant` \
             follows the read surface: omitted, the caller's own tenant; the answer repeats \
             it as a bare lowercase UUID. A setting nobody subscribed to at the tenant \
             answers 200 with no items. A caller the policy denies answers 403 for every key \
             and tenant; a tenant outside the caller's subtree (a standalone descendant and \
             the tenants below it included), an unknown key, a declaration outside the \
             caller's administrative domain and a setting hidden from the caller all answer \
             the same 404. A retired declaration is read like an active one. The read is a \
             single resource: `$filter`, `$orderby`, `$select`, `cursor` and `limit` answer \
             400. The per-change account of the reports is the setting's history.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .query_param(
            "tenant",
            false,
            "Tenant id; omitted, the caller's own tenant",
        )
        .handler(handlers::get_activation)
        .json_response_with_schema::<ActivationDto>(
            openapi,
            StatusCode::OK,
            "The activation states of the setting's current value at the tenant, one per \
             subscriber",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(resolver))
        .layer(axum::Extension(recorder))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "activation_routes_tests.rs"]
mod activation_routes_tests;

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
            "How the instances of each subscriber stand on the current value of a setting at \
             one tenant: one item per subscriber that has rows, ordered by subscriber, with \
             the `change_ref` of the change it answers, the `outcome` derived at the read \
             (`failed` over `incomplete` over `awaiting` over `succeeded`, never stored), \
             the counts `expected` (M, the instances the change expected), `confirmed` (N), \
             `failed`, `silent` and `silent_excluded`, and `instances`, one entry per \
             instance: `status` (`awaiting`, `succeeded`, `failed` or `silent`), `excluded`, \
             `reported_at` once settled, `silent_since` on a silent instance and on one a \
             late report settled, and `detail` on a `failed` one. An instance the change left \
             out because it was silent is listed after the rows with `excluded: true` and is \
             counted in `silent_excluded` only. The entries are ordered rows first, then \
             excluded, each part by status and then by `instance_id`, the same for every \
             caller, at most 64 per item. `instance_id` and `instance_label` are present only \
             for a caller whose own tenant is the root tenant and are absent, not masked, for \
             every other caller, the administrator who made the change included. The answer \
             carries no value, expected or applied, and no hash. `status` and `outcome` are \
             open sets: treat them as open sets and read an unknown one as not known to have \
             activated. An instance still outstanding after the report timeout is `silent` \
             and the outcome `incomplete`; it stays listed until it reports, is re-armed, a \
             later change replaces it, or it is forgotten a day after it was first called \
             silent. `tenant` follows the read surface: omitted, the caller's own tenant; the \
             answer repeats it as a bare lowercase UUID. A setting nobody subscribed to at \
             the tenant, or whose subscriptions no change has affected, answers 200 with no \
             items. A caller the policy denies answers 403 for every key and tenant; a \
             tenant outside the caller's subtree (a standalone descendant and the tenants \
             below it included), an unknown key, a declaration outside the caller's \
             administrative domain and a setting hidden from the caller all answer the same \
             404. A retired declaration is read like an active one. The read is a single \
             resource: `$filter`, `$orderby`, `$select`, `cursor` and `limit` answer 400. The \
             per-change account of the reports is the setting's history.",
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
            "The per-instance activation of the setting's current value at the tenant, one item \
             per subscriber",
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

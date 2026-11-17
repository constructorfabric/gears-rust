//! The twelve routes of DESIGN §3.3, one per SDK method, registered with
//! `OperationBuilder`.

use std::sync::Arc;

use axum::{Extension, Router, http::StatusCode};
use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::operation_builder::ParamSpec;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_gts::gts_id;

use super::dto;
use super::handlers;
use crate::domain::service::Service;

/// Platform base license; the gear has no feature of its own.
struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        gts_id!("cf.core.lic.feat.v1~cf.core.global.base.v1")
    }
}

impl toolkit::api::operation_builder::LicenseFeature for License {}

/// `If-Match` on `PATCH` (required) and `DELETE` (optional): the `ETag` of
/// the entry, or `*` for an unconditional write (DESIGN §3.3).
fn if_match_param(required: bool) -> ParamSpec {
    ParamSpec::header("If-Match")
        .required(required)
        .description(if required {
            "The ETag of the entry (`\"<version>\"`), or `*` for an unconditional update. A \
             missing header is rejected with `IF_MATCH_REQUIRED`; a stale one with `aborted` \
             and reason `OPTIMISTIC_LOCK_FAILURE`."
        } else {
            "The ETag of the entry (`\"<version>\"`); when present, the delete applies only to \
             that version."
        })
}

#[allow(clippy::too_many_lines)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<Service>,
) -> Router {
    // ==================== Resolution ====================

    router = OperationBuilder::get("/model-registry/v1/available-models/{canonical_id}")
        .operation_id("model_registry.resolve_model")
        .summary("Resolve a model for evaluation")
        .description(
            "Resolve a canonical id (`provider_slug::name`, percent-encoded as one path segment) in \
             the caller's effective catalog. Succeeds only when the model is visible, its provider is \
             enabled, it is approved, and the caller may evaluate it. Returns the full record, \
             `provider_settings`, `base_url`, and `secret_ref` included.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::resolve_model)
        .json_response_with_schema::<dto::ModelDto>(openapi, StatusCode::OK, "The model")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/available-models")
        .operation_id("model_registry.list_available_models")
        .summary("List the models available for evaluation")
        .description("Exactly the models the caller could resolve, as the same full records.")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_available_models)
        .json_response_with_schema::<toolkit_odata::Page<dto::ModelDto>>(
            openapi,
            StatusCode::OK,
            "A page of models",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // ==================== Providers ====================

    router = OperationBuilder::post("/model-registry/v1/providers")
        .operation_id("model_registry.create_provider")
        .summary("Create a provider")
        .description(
            "Create a provider in the caller's tenant. `slug` is immutable; `gts_type` changes only \
             through type migration.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateProviderRequestDto>(openapi, "Provider to create")
        .handler(handlers::create_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(
            openapi,
            StatusCode::CREATED,
            "Provider created",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.get_provider")
        .summary("Get an own or inherited provider")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(openapi, StatusCode::OK, "The provider")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/providers")
        .operation_id("model_registry.list_providers")
        .summary("List own and inherited providers")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_providers)
        .json_response_with_schema::<toolkit_odata::Page<dto::ProviderEntryDto>>(
            openapi,
            StatusCode::OK,
            "A page of providers",
        )
        .with_odata_filter::<ProviderFilterField>()
        .with_odata_orderby::<ProviderFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.update_provider")
        .summary("Update an own provider")
        .description(
            "Present fields replace the stored values; a `gts_type` different from the stored one \
             migrates the provider and all its models to the new type. Immutable fields are \
             rejected. Requires `If-Match`.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .param(if_match_param(true))
        .json_request::<dto::UpdateProviderRequestDto>(openapi, "Fields to change")
        .handler(handlers::update_provider)
        .json_response_with_schema::<dto::ProviderEntryDto>(
            openapi,
            StatusCode::OK,
            "Provider updated",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/model-registry/v1/providers/{id}")
        .operation_id("model_registry.delete_provider")
        .summary("Delete an own provider and its models")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .param(if_match_param(false))
        .handler(handlers::delete_provider)
        .no_content_response(StatusCode::NO_CONTENT, "Provider deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // ==================== Models ====================

    router = OperationBuilder::post("/model-registry/v1/models")
        .operation_id("model_registry.create_model")
        .summary("Create a model under an own provider")
        .description(
            "The gear builds `canonical_id`, copies `gts_type` from the provider, validates \
             `provider_settings` against the provider type schema, and records the approval decision.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateModelRequestDto>(openapi, "Model to create")
        .handler(handlers::create_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::CREATED, "Model created")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/models/{id}")
        .operation_id("model_registry.get_model")
        .summary("Get an own or inherited model, in any state")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::OK, "The model")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/model-registry/v1/models")
        .operation_id("model_registry.list_models")
        .summary("List own and inherited models in every state")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_models)
        .json_response_with_schema::<toolkit_odata::Page<dto::ModelEntryDto>>(
            openapi,
            StatusCode::OK,
            "A page of models",
        )
        .with_odata_filter::<ModelFilterField>()
        .with_odata_orderby::<ModelFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/model-registry/v1/models/{id}")
        .operation_id("model_registry.update_model")
        .summary("Update an own model")
        .description(
            "Present fields replace the stored values; a present `approval_state` records a decision; \
             a changed `provider_settings` is validated and a changed `secret_ref` is \
             ownership-checked. Immutable fields are rejected. Requires `If-Match`.",
        )
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .param(if_match_param(true))
        .json_request::<dto::UpdateModelRequestDto>(openapi, "Fields to change")
        .handler(handlers::update_model)
        .json_response_with_schema::<dto::ModelEntryDto>(openapi, StatusCode::OK, "Model updated")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/model-registry/v1/models/{id}")
        .operation_id("model_registry.delete_model")
        .summary("Delete an own model")
        .tag("Model Registry")
        .authenticated()
        .require_license_features::<License>([])
        .param(if_match_param(false))
        .handler(handlers::delete_model)
        .no_content_response(StatusCode::NO_CONTENT, "Model deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router.layer(Extension(service))
}

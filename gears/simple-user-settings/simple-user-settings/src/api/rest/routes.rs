use crate::api::rest::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmSettingsRepository;
use axum::http::StatusCode;
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

/// Type alias for the concrete service type.
pub type ConcreteService = Service<SeaOrmSettingsRepository>;

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    router = OperationBuilder::get("/simple-user-settings/v1/settings")
        .operation_id("simple_user_settings.get_settings")
        .summary("Get user settings")
        .description("Retrieve settings for the authenticated user")
        .tag("Settings")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_settings)
        .json_response_with_schema::<dto::SimpleUserSettingsDto>(
            openapi,
            StatusCode::OK,
            "Settings retrieved",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/simple-user-settings/v1/settings")
        .operation_id("simple_user_settings.update_settings")
        .summary("Update user settings")
        .description("Full update of user settings (POST semantics)")
        .tag("Settings")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateSimpleUserSettingsRequest>(openapi, "Settings update data")
        .handler(handlers::update_settings)
        .json_response_with_schema::<dto::SimpleUserSettingsDto>(
            openapi,
            StatusCode::OK,
            "Settings updated",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/simple-user-settings/v1/settings")
        .operation_id("simple_user_settings.patch_settings")
        .summary("Partially update user settings")
        .description("Partial update of user settings (PATCH semantics)")
        .tag("Settings")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::PatchSimpleUserSettingsRequest>(openapi, "Settings patch data")
        .handler(handlers::patch_settings)
        .json_response_with_schema::<dto::SimpleUserSettingsDto>(
            openapi,
            StatusCode::OK,
            "Settings patched",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = register_named_routes(router, openapi);

    router = router.layer(Extension(service));

    router
}

/// Keyed JSON values next to the fixed fields, one resource per key.
fn register_named_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    const KEY_DOC: &str = "Setting key: 1-128 characters from A-Z a-z 0-9 . _ - :";

    router = OperationBuilder::get("/simple-user-settings/v1/named-settings")
        .operation_id("simple_user_settings.list_named_settings")
        .summary("List named settings")
        .description("Every named setting of the authenticated user, ordered by key")
        .tag("Settings")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_named_settings)
        .json_response_with_schema::<dto::NamedSettingsListDto>(
            openapi,
            StatusCode::OK,
            "Named settings",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/simple-user-settings/v1/named-settings/{key}")
        .operation_id("simple_user_settings.get_named_setting")
        .summary("Get a named setting")
        .description("One named setting of the authenticated user; 404 if it is not set")
        .tag("Settings")
        .path_param("key", KEY_DOC)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_named_setting)
        .json_response_with_schema::<dto::NamedSettingDto>(openapi, StatusCode::OK, "Named setting")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::put("/simple-user-settings/v1/named-settings/{key}")
        .operation_id("simple_user_settings.put_named_setting")
        .summary("Set a named setting")
        .description(
            "Create or replace one named setting. The value is any JSON value within \
             the configured size bound; a new key past the per-user count bound is refused with 429.",
        )
        .tag("Settings")
        .path_param("key", KEY_DOC)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::PutNamedSettingRequest>(openapi, "The value to store")
        .handler(handlers::put_named_setting)
        .json_response_with_schema::<dto::NamedSettingDto>(
            openapi,
            StatusCode::OK,
            "Named setting stored",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_422(openapi)
        .error_429(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/simple-user-settings/v1/named-settings/{key}")
        .operation_id("simple_user_settings.delete_named_setting")
        .summary("Delete a named setting")
        .description("Forget one named setting. Deleting a key that is not set also answers 204.")
        .tag("Settings")
        .path_param("key", KEY_DOC)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_named_setting)
        .no_content_response(StatusCode::NO_CONTENT, "Named setting deleted (no body)")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/simple-user-settings/v1/named-settings")
        .operation_id("simple_user_settings.delete_all_named_settings")
        .summary("Delete all named settings")
        .description(
            "Forget every named setting of the authenticated user in one call, e.g. on \
             offboarding or an erasure request. Answers 204 whether or not any were set; \
             the fixed theme/language fields are not touched.",
        )
        .tag("Settings")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::delete_all_named_settings)
        .no_content_response(StatusCode::NO_CONTENT, "Named settings deleted (no body)")
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_license_as_ref() {
        let license = License;
        assert_eq!(license.as_ref(), CORE_GLOBAL_BASE_LICENSE_FEATURE);
    }

    #[test]
    fn test_license_implements_license_feature() {
        fn assert_license_feature<T: LicenseFeature>(_: &T) {}
        let license = License;
        assert_license_feature(&license);
    }
}

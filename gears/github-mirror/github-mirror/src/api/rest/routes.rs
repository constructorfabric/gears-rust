use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmGithubRepoRepository;

pub type ConcreteService = Service<SeaOrmGithubRepoRepository>;

const API_TAG: &str = "GitHub Mirror";

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
    router = OperationBuilder::get("/github-mirror/v1/health")
        .operation_id("github_mirror.health")
        .summary("GitHub Mirror health")
        .description("Reports that the github-mirror gear is loaded and serving requests")
        .tag(API_TAG)
        .anonymous()
        .handler(handlers::health)
        .json_response_with_schema::<dto::GithubMirrorHealthDto>(
            openapi,
            StatusCode::OK,
            "Gear is healthy",
        )
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos")
        .operation_id("github_mirror.list_repositories")
        .summary("List mirrored repositories")
        .description(
            "Returns the GitHub repositories held in the local mirror for the caller's tenant",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("limit", false, "Maximum number of repositories to return")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_repositories)
        .json_response_with_schema::<toolkit_odata::Page<dto::RepositoryDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored repositories",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(service))
}

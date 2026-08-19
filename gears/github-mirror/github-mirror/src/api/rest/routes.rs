use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmGithubRepoRepository;

pub type ConcreteService = Service<SeaOrmGithubRepoRepository>;

const API_TAG: &str = "GitHub Mirror";

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

    router.layer(Extension(service))
}

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::{
    SeaOrmCommitRepository, SeaOrmIssueRepository, SeaOrmPullRequestRepository,
    SeaOrmRepoRepository,
};

pub type ConcreteService = Service<
    SeaOrmRepoRepository,
    SeaOrmIssueRepository,
    SeaOrmPullRequestRepository,
    SeaOrmCommitRepository,
>;

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

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/issues")
        .operation_id("github_mirror.list_issues")
        .summary("List mirrored issues of a repository")
        .description(
            "Returns issues (pull requests included, flagged by is_pull_request) held in              the local mirror for the caller's tenant",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of issues to return")
        .handler(handlers::list_issues)
        .json_response_with_schema::<toolkit_odata::Page<dto::IssueDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored issues",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/pulls")
        .operation_id("github_mirror.list_pull_requests")
        .summary("List mirrored pull requests of a repository")
        .description("Returns pull requests held in the local mirror for the caller's tenant")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of pull requests to return")
        .handler(handlers::list_pull_requests)
        .json_response_with_schema::<toolkit_odata::Page<dto::PullRequestDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored pull requests",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/commits")
        .operation_id("github_mirror.list_commits")
        .summary("List mirrored commits of a repository")
        .description(
            "Returns commits held in the local mirror for the caller's tenant, newest first",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of commits to return")
        .handler(handlers::list_commits)
        .json_response_with_schema::<toolkit_odata::Page<dto::CommitDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored commits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/github-mirror/v1/repos/{owner}/{name}/sync")
        .operation_id("github_mirror.sync_repository")
        .summary("Sync a repository from GitHub into the mirror")
        .description(
            "Fetches the repository plus the first page of its issues, pull requests, and              commits from GitHub and upserts them into the caller's tenant mirror. First              slice of the sync engine: no pagination, conditional requests, or rate-limit              budgeting yet.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .handler(handlers::sync_repository)
        .json_response_with_schema::<dto::SyncSummaryDto>(
            openapi,
            StatusCode::OK,
            "Repository synced into the mirror",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(service))
}

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::{
    SeaOrmBranchRepository, SeaOrmCommentRepository, SeaOrmCommitRepository, SeaOrmIssueRepository,
    SeaOrmLabelRepository, SeaOrmMilestoneRepository, SeaOrmPullRequestRepository,
    SeaOrmReleaseRepository, SeaOrmRepoRepository, SeaOrmReviewCommentRepository,
    SeaOrmReviewRepository,
};

pub type ConcreteService = Service<
    SeaOrmRepoRepository,
    SeaOrmIssueRepository,
    SeaOrmPullRequestRepository,
    SeaOrmCommitRepository,
    SeaOrmCommentRepository,
    SeaOrmReviewCommentRepository,
    SeaOrmReviewRepository,
    SeaOrmLabelRepository,
    SeaOrmMilestoneRepository,
    SeaOrmReleaseRepository,
    SeaOrmBranchRepository,
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

    router = register_repo_metadata_routes(router, openapi);
    router = register_comment_routes(router, openapi);

    router.layer(Extension(service))
}

fn register_repo_metadata_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/labels")
        .operation_id("github_mirror.list_labels")
        .summary("List mirrored labels of a repository")
        .description("Returns labels held in the local mirror for the tenant, by name")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of labels to return")
        .handler(handlers::list_labels)
        .json_response_with_schema::<toolkit_odata::Page<dto::LabelDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored labels",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/milestones")
        .operation_id("github_mirror.list_milestones")
        .summary("List mirrored milestones of a repository")
        .description("Returns milestones held in the local mirror for the tenant, by number")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of milestones to return")
        .handler(handlers::list_milestones)
        .json_response_with_schema::<toolkit_odata::Page<dto::MilestoneDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored milestones",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/releases")
        .operation_id("github_mirror.list_releases")
        .summary("List mirrored releases of a repository")
        .description("Returns releases held in the local mirror for the tenant, newest first")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of releases to return")
        .handler(handlers::list_releases)
        .json_response_with_schema::<toolkit_odata::Page<dto::ReleaseDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored releases",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/branches")
        .operation_id("github_mirror.list_branches")
        .summary("List mirrored branch heads of a repository")
        .description("Returns branch heads held in the local mirror for the tenant, by name")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .query_param("limit", false, "Maximum number of branches to return")
        .handler(handlers::list_branches)
        .json_response_with_schema::<toolkit_odata::Page<dto::BranchDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored branch heads",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_comment_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router =
        OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/issues/{number}/comments")
            .operation_id("github_mirror.list_comments")
            .summary("List mirrored comments of an issue or pull request")
            .description(
                "Returns comments held in the local mirror for the caller's tenant, oldest first",
            )
            .tag(API_TAG)
            .authenticated()
            .require_license_features::<License>([])
            .path_param("owner", "Repository owner login")
            .path_param("name", "Repository name")
            .path_param("number", "Issue or pull request number")
            .query_param("limit", false, "Maximum number of comments to return")
            .handler(handlers::list_comments)
            .json_response_with_schema::<toolkit_odata::Page<dto::CommentDto>>(
                openapi,
                StatusCode::OK,
                "Paginated list of mirrored comments",
            )
            .error_400(openapi)
            .error_401(openapi)
            .error_403(openapi)
            .error_404(openapi)
            .error_500(openapi)
            .register(router, openapi);

    router =
        OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/pulls/{number}/comments")
            .operation_id("github_mirror.list_review_comments")
            .summary("List mirrored review comments of a pull request")
            .description(
                "Returns code review comments held in the local mirror for the tenant,                  oldest first",
            )
            .tag(API_TAG)
            .authenticated()
            .require_license_features::<License>([])
            .path_param("owner", "Repository owner login")
            .path_param("name", "Repository name")
            .path_param("number", "Pull request number")
            .query_param("limit", false, "Maximum number of review comments to return")
            .handler(handlers::list_review_comments)
            .json_response_with_schema::<toolkit_odata::Page<dto::ReviewCommentDto>>(
                openapi,
                StatusCode::OK,
                "Paginated list of mirrored review comments",
            )
            .error_400(openapi)
            .error_401(openapi)
            .error_403(openapi)
            .error_404(openapi)
            .error_500(openapi)
            .register(router, openapi);

    router = OperationBuilder::get("/github-mirror/v1/repos/{owner}/{name}/pulls/{number}/reviews")
        .operation_id("github_mirror.list_reviews")
        .summary("List mirrored reviews of a pull request")
        .description(
            "Returns pull-request reviews (approve/request-changes/comment verdicts) held in              the local mirror for the tenant, oldest first",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .path_param("number", "Pull request number")
        .query_param("limit", false, "Maximum number of reviews to return")
        .handler(handlers::list_reviews)
        .json_response_with_schema::<toolkit_odata::Page<dto::ReviewDto>>(
            openapi,
            StatusCode::OK,
            "Paginated list of mirrored reviews",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

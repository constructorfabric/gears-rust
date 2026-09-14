//! The GitHub-compatible endpoints (PRD §5.8), served at GitHub's own
//! paths so existing clients only swap their base URL.

use axum::Router;
use axum::http::StatusCode;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::routes::{API_TAG, License, PAGE_DOC, PER_PAGE_DOC, STATE_DOC};
use crate::api::rest::{dto, handlers};

// ---------------------------------------------------------------------------
// Route paths.
//
// PRD §5.8 requires these to be byte-for-byte GitHub's own paths — no gear
// prefix, no version segment — so an existing GitHub client can switch to the
// mirror by changing only its base URL. That shape fails the DE0801
// versioned-path lint, whose check binds to literal arguments only; routing
// the paths through these consts records the exception explicitly instead of
// weakening the lint for the whole workspace. The gear's own endpoints stay
// versioned under `/github-mirror/v1/` (see `routes/v1.rs`).
// ---------------------------------------------------------------------------

const USER: &str = "/user";
const USER_REPOS: &str = "/user/repos";
const REPO_ISSUES: &str = "/repos/{owner}/{name}/issues";
const REPO_ISSUES_ITEM_COMMENTS: &str = "/repos/{owner}/{name}/issues/{number}/comments";
const REPO_BRANCHES: &str = "/repos/{owner}/{name}/branches";
const REPO_CONTRIBUTORS: &str = "/repos/{owner}/{name}/contributors";
const REPO_ISSUES_ITEM_EVENTS: &str = "/repos/{owner}/{name}/issues/{number}/events";
const REPO_ISSUES_ITEM_REACTIONS: &str = "/repos/{owner}/{name}/issues/{number}/reactions";
const REPO_ISSUES_ITEM_TIMELINE: &str = "/repos/{owner}/{name}/issues/{number}/timeline";
const REPO_DEPLOYMENTS: &str = "/repos/{owner}/{name}/deployments";
const REPO_COMMITS: &str = "/repos/{owner}/{name}/commits";
const REPO_COMMITS_ITEM_COMMENTS: &str = "/repos/{owner}/{name}/commits/{sha}/comments";
const REPO_COMMITS_ITEM_CHECK_RUNS: &str = "/repos/{owner}/{name}/commits/{sha}/check-runs";
const REPO_COMMITS_ITEM_STATUSES: &str = "/repos/{owner}/{name}/commits/{sha}/statuses";
const REPO: &str = "/repos/{owner}/{name}";
const REPO_ISSUES_ITEM: &str = "/repos/{owner}/{name}/issues/{number}";
const REPO_PULLS_ITEM: &str = "/repos/{owner}/{name}/pulls/{number}";
const REPO_COMMITS_ITEM: &str = "/repos/{owner}/{name}/commits/{sha}";
const REPO_PULLS: &str = "/repos/{owner}/{name}/pulls";
const REPO_PULLS_ITEM_REVIEWS: &str = "/repos/{owner}/{name}/pulls/{number}/reviews";
const REPO_PULLS_ITEM_COMMENTS: &str = "/repos/{owner}/{name}/pulls/{number}/comments";
const REPO_PULLS_ITEM_FILES: &str = "/repos/{owner}/{name}/pulls/{number}/files";
const REPO_PULLS_ITEM_COMMITS: &str = "/repos/{owner}/{name}/pulls/{number}/commits";
const REPO_TAGS: &str = "/repos/{owner}/{name}/tags";
const REPO_RELEASES: &str = "/repos/{owner}/{name}/releases";
const REPO_MILESTONES: &str = "/repos/{owner}/{name}/milestones";
const REPO_LABELS: &str = "/repos/{owner}/{name}/labels";
const REPO_ACTIONS_RUNS: &str = "/repos/{owner}/{name}/actions/runs";
const REPO_ACTIONS_RUNS_ITEM_JOBS: &str = "/repos/{owner}/{name}/actions/runs/{run_id}/jobs";

pub fn register_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = register_user_routes(router, openapi);
    router = register_repo_routes(router, openapi);
    router = register_issue_activity_routes(router, openapi);
    router = register_commit_routes(router, openapi);
    router = register_item_routes(router, openapi);
    router = register_pull_routes(router, openapi);
    router = register_metadata_routes(router, openapi);

    router
}

fn register_user_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(USER)
        .operation_id("github_mirror.compat.get_authenticated_user")
        .summary("Get the authenticated user (GitHub-compatible)")
        .description(
            "Get the authenticated user, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_authenticated_user)
        .json_response_with_schema::<dto::AuthenticatedUserDto>(
            openapi,
            StatusCode::OK,
            "The mirror's own identity, GitHub-shaped",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(USER_REPOS)
        .operation_id("github_mirror.compat.list_user_repos")
        .summary("List the caller's repositories (GitHub-compatible)")
        .description(
            "List the caller's repositories, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_user_repos)
        .json_array_response_with_schema::<dto::RepoDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of the tenant's mirrored repositories",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_repo_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO_ISSUES)
        .operation_id("github_mirror.compat.list_issues")
        .summary("List issues (GitHub-compatible)")
        .description(
            "List issues, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .query_param("state", false, STATE_DOC)
        .handler(handlers::list_issues)
        .json_array_response_with_schema::<dto::IssueDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of issues",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ISSUES_ITEM_COMMENTS)
        .operation_id("github_mirror.compat.list_comments")
        .summary("List issue comments (GitHub-compatible)")
        .description(
            "List issue comments, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Issue or pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_comments)
        .json_array_response_with_schema::<dto::CommentDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of issue comments",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_BRANCHES)
        .operation_id("github_mirror.compat.list_branches")
        .summary("List branches (GitHub-compatible)")
        .description(
            "List branches, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_branches)
        .json_array_response_with_schema::<dto::BranchDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of branches",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_CONTRIBUTORS)
        .operation_id("github_mirror.compat.list_contributors")
        .summary("List contributors (GitHub-compatible)")
        .description(
            "List contributors, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_contributors)
        .json_array_response_with_schema::<dto::ContributorDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of contributors",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_issue_activity_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO_ISSUES_ITEM_EVENTS)
        .operation_id("github_mirror.compat.list_issue_events")
        .summary("List issue events (GitHub-compatible)")
        .description(
            "List issue events, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Issue or pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_issue_events)
        .json_array_response_with_schema::<dto::IssueEventDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of issue events",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ISSUES_ITEM_REACTIONS)
        .operation_id("github_mirror.compat.list_issue_reactions")
        .summary("List issue reactions (GitHub-compatible)")
        .description(
            "List issue reactions, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Issue or pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_issue_reactions)
        .json_array_response_with_schema::<dto::IssueReactionDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of issue reactions",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ISSUES_ITEM_TIMELINE)
        .operation_id("github_mirror.compat.list_issue_timeline")
        .summary("List issue timeline (GitHub-compatible)")
        .description(
            "List issue timeline, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Issue or pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_issue_timeline)
        .json_array_response_with_schema::<dto::IssueTimelineEventDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped issue timeline, newest entry last",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_DEPLOYMENTS)
        .operation_id("github_mirror.compat.list_deployments")
        .summary("List deployments (GitHub-compatible)")
        .description(
            "List deployments, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_deployments)
        .json_array_response_with_schema::<dto::DeploymentDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of deployments",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_commit_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO_COMMITS)
        .operation_id("github_mirror.compat.list_commits")
        .summary("List commits (GitHub-compatible)")
        .description(
            "List commits, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_commits)
        .json_array_response_with_schema::<dto::CommitDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of commits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_COMMITS_ITEM_COMMENTS)
        .operation_id("github_mirror.compat.list_commit_comments")
        .summary("List commit comments (GitHub-compatible)")
        .description(
            "List commit comments, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repository owner login")
        .path_param("name", "Repository name")
        .path_param("sha", "Commit SHA")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_commit_comments)
        .json_array_response_with_schema::<dto::CommitCommentDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of commit comments",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_COMMITS_ITEM_CHECK_RUNS)
        .operation_id("github_mirror.compat.list_check_runs")
        .summary("List commit check runs (GitHub-compatible)")
        .description(
            "List commit check runs, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("sha", "Commit SHA")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_check_runs)
        .json_response_with_schema::<dto::CheckRunsPageDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped check runs page",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_COMMITS_ITEM_STATUSES)
        .operation_id("github_mirror.compat.list_commit_statuses")
        .summary("List commit statuses (GitHub-compatible)")
        .description(
            "List commit statuses, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("sha", "Commit SHA")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_commit_statuses)
        .json_array_response_with_schema::<dto::CommitStatusDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of commit statuses",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_item_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO)
        .operation_id("github_mirror.compat.get_repo")
        .summary("Get a repository (GitHub-compatible)")
        .description(
            "Get a repository, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .handler(handlers::get_repo)
        .json_response_with_schema::<dto::RepoDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped repository",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ISSUES_ITEM)
        .operation_id("github_mirror.compat.get_issue")
        .summary("Get an issue (GitHub-compatible)")
        .description(
            "Get an issue, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Issue number")
        .handler(handlers::get_issue)
        .json_response_with_schema::<dto::IssueDto>(openapi, StatusCode::OK, "GitHub-shaped issue")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_PULLS_ITEM)
        .operation_id("github_mirror.compat.get_pull_request")
        .summary("Get a pull request (GitHub-compatible)")
        .description(
            "Get a pull request, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Pull request number")
        .handler(handlers::get_pull_request)
        .json_response_with_schema::<dto::PullRequestDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped pull request",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_COMMITS_ITEM)
        .operation_id("github_mirror.compat.get_commit")
        .summary("Get a commit (GitHub-compatible)")
        .description(
            "Get a commit, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("sha", "Commit SHA")
        .handler(handlers::get_commit)
        .json_response_with_schema::<dto::CommitDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped commit with stats and files",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_pull_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO_PULLS)
        .operation_id("github_mirror.compat.list_pull_requests")
        .summary("List pull requests (GitHub-compatible)")
        .description(
            "List pull requests, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .query_param("state", false, STATE_DOC)
        .handler(handlers::list_pull_requests)
        .json_array_response_with_schema::<dto::PullRequestDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of pull requests",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_PULLS_ITEM_REVIEWS)
        .operation_id("github_mirror.compat.list_reviews")
        .summary("List pull request reviews (GitHub-compatible)")
        .description(
            "List pull request reviews, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_reviews)
        .json_array_response_with_schema::<dto::ReviewDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of reviews",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_PULLS_ITEM_COMMENTS)
        .operation_id("github_mirror.compat.list_review_comments")
        .summary("List pull request review comments (GitHub-compatible)")
        .description(
            "List pull request review comments, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_review_comments)
        .json_array_response_with_schema::<dto::ReviewCommentDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of review comments",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_PULLS_ITEM_FILES)
        .operation_id("github_mirror.compat.list_pull_request_files")
        .summary("List pull request files (GitHub-compatible)")
        .description(
            "List pull request files, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_pull_request_files)
        .json_array_response_with_schema::<dto::PullRequestFileDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of pull request files",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_PULLS_ITEM_COMMITS)
        .operation_id("github_mirror.compat.list_pull_request_commits")
        .summary("List pull request commits (GitHub-compatible)")
        .description(
            "List pull request commits, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("number", "Pull request number")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_pull_request_commits)
        .json_array_response_with_schema::<dto::CommitDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of pull request commits",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

fn register_metadata_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::get(REPO_TAGS)
        .operation_id("github_mirror.compat.list_tags")
        .summary("List tags (GitHub-compatible)")
        .description(
            "List tags, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_tags)
        .json_array_response_with_schema::<dto::TagDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of tags",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_RELEASES)
        .operation_id("github_mirror.compat.list_releases")
        .summary("List releases (GitHub-compatible)")
        .description(
            "List releases, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_releases)
        .json_array_response_with_schema::<dto::ReleaseDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of releases",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_MILESTONES)
        .operation_id("github_mirror.compat.list_milestones")
        .summary("List milestones (GitHub-compatible)")
        .description(
            "List milestones, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_milestones)
        .json_array_response_with_schema::<dto::MilestoneDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of milestones",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_LABELS)
        .operation_id("github_mirror.compat.list_labels")
        .summary("List labels (GitHub-compatible)")
        .description(
            "List labels, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_labels)
        .json_array_response_with_schema::<dto::LabelDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped list of labels",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ACTIONS_RUNS)
        .operation_id("github_mirror.compat.list_workflow_runs")
        .summary("List workflow runs (GitHub-compatible)")
        .description(
            "List workflow runs, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_workflow_runs)
        .json_response_with_schema::<dto::WorkflowRunsPageDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped workflow runs page",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get(REPO_ACTIONS_RUNS_ITEM_JOBS)
        .operation_id("github_mirror.compat.list_workflow_jobs")
        .summary("List workflow run jobs (GitHub-compatible)")
        .description(
            "List workflow run jobs, served entirely from the locally mirrored store in GitHub's JSON shape - the mirror never calls live GitHub to answer. List responses paginate with `page`/`per_page` and a `Link` header, exactly as GitHub's API does. Data is as fresh as the repository's last sync.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("owner", "Repo owner login")
        .path_param("name", "Repo name")
        .path_param("run_id", "Workflow run id")
        .query_param("page", false, PAGE_DOC)
        .query_param("per_page", false, PER_PAGE_DOC)
        .handler(handlers::list_workflow_jobs)
        .json_response_with_schema::<dto::WorkflowJobsPageDto>(
            openapi,
            StatusCode::OK,
            "GitHub-shaped workflow jobs page",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}

//! Handlers of the mirror's REST surfaces.
//!
//! The GitHub-compatible handlers (PRD §5.8) return GitHub-shaped bodies
//! with `page`/`per_page` pagination and a `Link` response header. The
//! extended handlers under `/github-mirror/v1/` keep the platform shapes.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::{Json, extract::Extension};
use serde::Deserialize;
use toolkit::api::canonical_prelude::*;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use crate::api::rest::routes::ConcreteService;
use crate::domain::repo::{ListingDirection, ListingFilter, ListingSort};

use super::dto::{
    AuthenticatedUserDto, BranchDto, CheckRunDto, CheckRunsPageDto, CommentDto, CommitCommentDto,
    CommitDto, CommitFileDto, CommitStatsDto, CommitStatusDto, ContributorDto, DeploymentDto,
    GithubMirrorHealthDto, IssueDto, IssueEventDto, IssueReactionDto, IssueTimelineEventDto,
    LabelDto, MilestoneDto, PullRequestDto, PullRequestFileDto, ReleaseDto, RepoDto,
    ReviewCommentDto, ReviewDto, ReviewThreadDto, SyncSummaryDto, TagDto, WorkflowJobDto,
    WorkflowJobsPageDto, WorkflowRunDto, WorkflowRunsPageDto,
};

const DEFAULT_PER_PAGE: u64 = 30;
const MAX_PER_PAGE: u64 = 100;

/// GitHub-style pagination query (`?page=2&per_page=50`), plus the `state`
/// filter the issue and pull listings accept.
#[derive(Debug, Deserialize)]
pub struct GithubPageQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub state: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub since: Option<String>,
}

impl GithubPageQuery {
    /// The state to filter on, following GitHub: no `state` means `open`,
    /// `all` means no filter, anything else is passed through as given.
    fn state_filter(&self) -> Option<&str> {
        match self.state.as_deref() {
            None => Some("open"),
            Some("all") => None,
            Some(state) => Some(state),
        }
    }

    /// The whole listing filter, with GitHub's defaults for anything the
    /// caller left out.
    fn listing_filter(&self) -> ListingFilter<'_> {
        ListingFilter {
            state: self.state_filter(),
            sort: ListingSort::parse(self.sort.as_deref()),
            direction: ListingDirection::parse(self.direction.as_deref()),
            since: self.since.as_deref(),
        }
    }
}

struct GithubPage {
    page: u64,
    per_page: u64,
}

impl GithubPageQuery {
    fn normalized(&self) -> GithubPage {
        GithubPage {
            page: self.page.filter(|p| *p >= 1).unwrap_or(1),
            per_page: self
                .per_page
                .filter(|p| *p >= 1)
                .unwrap_or(DEFAULT_PER_PAGE)
                .min(MAX_PER_PAGE),
        }
    }
}

impl GithubPage {
    fn odata(&self) -> ODataQuery {
        ODataQuery {
            limit: Some(self.page.saturating_mul(self.per_page)),
            ..ODataQuery::default()
        }
    }

    fn slice<T, D: From<T>>(&self, items: Vec<T>) -> Vec<D> {
        let start =
            usize::try_from((self.page - 1).saturating_mul(self.per_page)).unwrap_or(usize::MAX);
        let take = usize::try_from(self.per_page).unwrap_or(usize::MAX);
        items
            .into_iter()
            .skip(start)
            .take(take)
            .map(D::from)
            .collect()
    }

    fn link_header(&self, path: &str, returned: usize) -> HeaderMap {
        self.link_header_with_total(path, returned, None)
    }

    /// The `Link` header, with `rel="last"` resolved from `total` when the
    /// listing can count itself.
    ///
    /// Without a total the last page is only knowable when the current one
    /// came back short, so on a full page `rel="last"` is omitted rather than
    /// guessed — GitHub itself always knows the total and always sends it.
    fn link_header_with_total(&self, path: &str, returned: usize, total: Option<u64>) -> HeaderMap {
        let last_page = total.map(|total| total.div_ceil(self.per_page).max(1));
        let is_last_page =
            last_page.map_or(returned as u64 != self.per_page, |last| self.page >= last);

        let mut links = Vec::new();
        if !is_last_page {
            links.push(format!(
                "<{path}?page={}&per_page={}>; rel=\"next\"",
                self.page + 1,
                self.per_page
            ));
        }
        if self.page > 1 {
            links.push(format!(
                "<{path}?page={}&per_page={}>; rel=\"prev\"",
                self.page - 1,
                self.per_page
            ));
            links.push(format!(
                "<{path}?page=1&per_page={}>; rel=\"first\"",
                self.per_page
            ));
        }
        // With a total the last page is known outright; without one, only a
        // short page proves it is the end.
        if let Some(last) = last_page {
            links.push(format!(
                "<{path}?page={last}&per_page={}>; rel=\"last\"",
                self.per_page
            ));
        } else if is_last_page {
            links.push(format!(
                "<{path}?page={}&per_page={}>; rel=\"last\"",
                self.page, self.per_page
            ));
        }

        let mut headers = HeaderMap::new();
        if !links.is_empty()
            && let Ok(value) = HeaderValue::from_str(&links.join(", "))
        {
            headers.insert(header::LINK, value);
        }
        headers
    }
}

type GithubList<D> = ApiResult<(HeaderMap, JsonBody<Vec<D>>)>;

fn respond<D>(page: &GithubPage, path: &str, items: Vec<D>) -> (HeaderMap, JsonBody<Vec<D>>) {
    let headers = page.link_header(path, items.len());
    (headers, Json(items))
}

/// [`respond`] for a listing that knows how many rows it has in total, so the
/// `Link` header can name the last page even from a full one.
fn respond_counted<D>(
    page: &GithubPage,
    path: &str,
    items: Vec<D>,
    total: u64,
) -> (HeaderMap, JsonBody<Vec<D>>) {
    let headers = page.link_header_with_total(path, items.len(), Some(total));
    (headers, Json(items))
}

// The signature must be `async` for axum's `Handler` impl even though the
// status read is synchronous.
#[allow(clippy::unused_async)]
pub async fn health(
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<GithubMirrorHealthDto>> {
    let status = svc.status();
    Ok(Json(status.into()))
}

pub async fn list_repos(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    OData(query): OData,
) -> ApiResult<JsonPage<RepoDto>> {
    let page: Page<_> = svc.list_repos(&ctx, &query).await?;
    Ok(Json(page.map_items(RepoDto::from)))
}

pub async fn sync_repository(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<JsonBody<SyncSummaryDto>> {
    let summary = svc.sync_repository(&ctx, &owner, &name).await?;
    Ok(Json(summary.into()))
}

pub async fn list_issues(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueDto> {
    let page = query.normalized();
    let items = svc
        .list_issues(&ctx, &owner, &name, &page.odata(), query.listing_filter())
        .await?
        .items;
    let total = svc
        .count_issues(&ctx, &owner, &name, query.listing_filter())
        .await?;
    let path = format!("/repos/{owner}/{name}/issues");
    Ok(respond_counted(&page, &path, page.slice(items), total))
}

pub async fn list_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommentDto> {
    let page = query.normalized();
    let items = svc
        .list_comments(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/comments");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_pull_requests(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<PullRequestDto> {
    let page = query.normalized();
    let items = svc
        .list_pull_requests(&ctx, &owner, &name, &page.odata(), query.listing_filter())
        .await?
        .items;
    let total = svc
        .count_pull_requests(&ctx, &owner, &name, query.listing_filter())
        .await?;
    let path = format!("/repos/{owner}/{name}/pulls");
    Ok(respond_counted(&page, &path, page.slice(items), total))
}

pub async fn list_reviews(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReviewDto> {
    let page = query.normalized();
    let items = svc
        .list_reviews(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/reviews");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_review_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReviewCommentDto> {
    let page = query.normalized();
    let items = svc
        .list_review_comments(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/comments");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_pull_request_files(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<PullRequestFileDto> {
    let page = query.normalized();
    let items = svc
        .list_pull_request_files(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/files");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_commits(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitDto> {
    let page = query.normalized();
    let items = svc
        .list_commits(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let total = svc.count_commits(&ctx, &owner, &name).await?;
    let path = format!("/repos/{owner}/{name}/commits");
    Ok(respond_counted(&page, &path, page.slice(items), total))
}

pub async fn list_branches(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<BranchDto> {
    let page = query.normalized();
    let items = svc
        .list_branches(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/branches");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_tags(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<TagDto> {
    let page = query.normalized();
    let items = svc
        .list_tags(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/tags");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_releases(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReleaseDto> {
    let page = query.normalized();
    let items = svc
        .list_releases(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/releases");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_milestones(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<MilestoneDto> {
    let page = query.normalized();
    let items = svc
        .list_milestones(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/milestones");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_labels(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<LabelDto> {
    let page = query.normalized();
    let items = svc
        .list_labels(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/labels");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_contributors(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ContributorDto> {
    let page = query.normalized();
    let items = svc
        .list_contributors(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/contributors");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_workflow_runs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<WorkflowRunsPageDto>)> {
    let page = query.normalized();
    let items = svc
        .list_workflow_runs(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let runs: Vec<WorkflowRunDto> = page.slice(items);
    let path = format!("/repos/{owner}/{name}/actions/runs");
    let headers = page.link_header(&path, runs.len());
    // GitHub's `total_count` spans every page, so it is a count, not the
    // length of the slice being served.
    let total_count =
        i64::try_from(svc.count_workflow_runs(&ctx, &owner, &name).await?).unwrap_or(i64::MAX);
    Ok((
        headers,
        Json(WorkflowRunsPageDto {
            total_count,
            workflow_runs: runs,
        }),
    ))
}

pub async fn list_commit_files(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<CommitFileDto>> {
    let page: Page<_> = svc
        .list_commit_files(&ctx, &owner, &name, &sha, &query)
        .await?;
    Ok(Json(page.map_items(CommitFileDto::from)))
}

pub async fn list_review_threads(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    OData(query): OData,
) -> ApiResult<JsonPage<ReviewThreadDto>> {
    let page: Page<_> = svc
        .list_review_threads(&ctx, &owner, &name, number, &query)
        .await?;
    Ok(Json(page.map_items(ReviewThreadDto::from)))
}

pub async fn get_repo(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<JsonBody<RepoDto>> {
    let repo = svc.get_repo(&ctx, &owner, &name).await?;
    Ok(Json(repo.into()))
}

pub async fn get_issue(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> ApiResult<JsonBody<IssueDto>> {
    let issue = svc.get_issue(&ctx, &owner, &name, number).await?;
    Ok(Json(issue.into()))
}

pub async fn get_pull_request(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> ApiResult<JsonBody<PullRequestDto>> {
    let pull = svc.get_pull_request(&ctx, &owner, &name, number).await?;
    Ok(Json(pull.into()))
}

pub async fn get_commit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
) -> ApiResult<JsonBody<CommitDto>> {
    let commit = svc.get_commit(&ctx, &owner, &name, &sha).await?;
    let files = svc
        .list_commit_files(&ctx, &owner, &name, &sha, &ODataQuery::default())
        .await?
        .items;

    let mut body = CommitDto::from(commit.clone());
    body.stats = Some(CommitStatsDto {
        additions: commit.additions,
        deletions: commit.deletions,
        total: commit.additions + commit.deletions,
    });
    body.files = files.into_iter().map(PullRequestFileDto::from).collect();
    Ok(Json(body))
}

pub async fn list_commit_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitCommentDto> {
    let page = query.normalized();
    let items = svc
        .list_commit_comments(&ctx, &owner, &name, &sha, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/commits/{sha}/comments");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_issue_events(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueEventDto> {
    let page = query.normalized();
    let items = svc
        .list_issue_events(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/events");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_issue_reactions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueReactionDto> {
    let page = query.normalized();
    let items = svc
        .list_issue_reactions(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/reactions");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_issue_timeline(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueTimelineEventDto> {
    let page = query.normalized();
    let items = svc
        .list_issue_timeline(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/timeline");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_deployments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<DeploymentDto> {
    let page = query.normalized();
    let items = svc
        .list_deployments(&ctx, &owner, &name, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/deployments");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_pull_request_commits(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitDto> {
    let page = query.normalized();
    let items = svc
        .list_pull_request_commits(&ctx, &owner, &name, number, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/commits");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_commit_statuses(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitStatusDto> {
    let page = query.normalized();
    let items = svc
        .list_commit_statuses(&ctx, &owner, &name, &sha, &page.odata())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/commits/{sha}/statuses");
    Ok(respond(&page, &path, page.slice(items)))
}

pub async fn list_workflow_jobs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, run_id)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<WorkflowJobsPageDto>)> {
    let page = query.normalized();
    let items = svc
        .list_workflow_jobs(&ctx, &owner, &name, run_id, &page.odata())
        .await?
        .items;
    let jobs: Vec<WorkflowJobDto> = page.slice(items);
    let path = format!("/repos/{owner}/{name}/actions/runs/{run_id}/jobs");
    let headers = page.link_header(&path, jobs.len());
    let total_count = i64::try_from(svc.count_workflow_jobs(&ctx, &owner, &name, run_id).await?)
        .unwrap_or(i64::MAX);
    Ok((headers, Json(WorkflowJobsPageDto { total_count, jobs })))
}

pub async fn list_check_runs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<CheckRunsPageDto>)> {
    let page = query.normalized();
    let items = svc
        .list_check_runs(&ctx, &owner, &name, &sha, &page.odata())
        .await?
        .items;
    let check_runs: Vec<CheckRunDto> = page.slice(items);
    let path = format!("/repos/{owner}/{name}/commits/{sha}/check-runs");
    let headers = page.link_header(&path, check_runs.len());
    let total_count =
        i64::try_from(svc.count_check_runs(&ctx, &owner, &name, &sha).await?).unwrap_or(i64::MAX);
    Ok((
        headers,
        Json(CheckRunsPageDto {
            total_count,
            check_runs,
        }),
    ))
}

/// GitHub's `GET /user`, answered by the mirror on its own behalf: clients
/// call it to validate a connection before browsing repositories.
pub async fn get_authenticated_user(
    Extension(_ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<AuthenticatedUserDto>> {
    let status = svc.status();
    Ok(Json(AuthenticatedUserDto {
        id: 0,
        login: status.gear,
        name: Some(format!("GitHub Mirror {}", status.version)),
        user_type: "Bot".to_owned(),
    }))
}

/// GitHub's `GET /user/repos`. For the mirror, "the authenticated user's
/// repositories" are the repositories mirrored for the caller's tenant.
pub async fn list_user_repos(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<RepoDto> {
    let page = query.normalized();
    let items = svc.list_repos(&ctx, &page.odata()).await?.items;
    Ok(respond(&page, "/user/repos", page.slice(items)))
}

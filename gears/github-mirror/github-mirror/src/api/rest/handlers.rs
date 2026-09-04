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
use chrono::{DateTime, Utc};

use crate::domain::error::DomainError;
use crate::domain::repo::{IssueState, ListingDirection, ListingFilter, ListingSort, PageWindow};

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
/// Furthest row page-based paging may reach.
///
/// GitHub answers `page * per_page > 1000` with a 422 telling the caller to
/// switch to cursor paging, so the mirror stops in the same place: it keeps
/// the surfaces identical and stops an arbitrary `?page=` from becoming an
/// arbitrary SQL `OFFSET`.
const MAX_PAGED_ROWS: u64 = 1_000;

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
    /// `all` means no filter.
    ///
    /// # Errors
    /// `Validation` when the value is not a state GitHub knows.
    fn state_filter(&self) -> Result<Option<IssueState>, DomainError> {
        match self.state.as_deref() {
            None => Ok(Some(IssueState::Open)),
            Some("all") => Ok(None),
            Some(state) => IssueState::parse(state).map(Some),
        }
    }

    fn since_filter(&self) -> Result<Option<DateTime<Utc>>, DomainError> {
        self.since
            .as_deref()
            .map(|raw| {
                DateTime::parse_from_rfc3339(raw)
                    .map(|at| at.with_timezone(&Utc))
                    .map_err(|e| DomainError::Validation {
                        field: "since".to_owned(),
                        message: format!("`{raw}` is not an RFC3339 timestamp: {e}"),
                    })
            })
            .transpose()
    }

    /// The whole listing filter, with GitHub's defaults for anything the
    /// caller left out.
    ///
    /// # Errors
    /// Whatever [`Self::state_filter`] or [`Self::since_filter`] returns.
    fn listing_filter(&self) -> Result<ListingFilter, DomainError> {
        Ok(ListingFilter {
            state: self.state_filter()?,
            sort: ListingSort::parse(self.sort.as_deref()),
            direction: ListingDirection::parse(self.direction.as_deref()),
            since: self.since_filter()?,
        })
    }
}

struct GithubPage {
    page: u64,
    per_page: u64,
}

impl GithubPageQuery {
    /// # Errors
    /// `Validation` when the requested page reaches past
    /// [`MAX_PAGED_ROWS`], which is where GitHub itself stops.
    fn normalized(&self) -> Result<GithubPage, DomainError> {
        let page = self.page.filter(|p| *p >= 1).unwrap_or(1);
        let per_page = self
            .per_page
            .filter(|p| *p >= 1)
            .unwrap_or(DEFAULT_PER_PAGE)
            .min(MAX_PER_PAGE);

        if page.saturating_mul(per_page) > MAX_PAGED_ROWS {
            return Err(DomainError::Validation {
                field: "page".to_owned(),
                message: concat!(
                    "Pagination with the page parameter is not supported for large datasets, ",
                    "please use cursor based pagination (after/before)"
                )
                .to_owned(),
            });
        }

        Ok(GithubPage { page, per_page })
    }
}

impl GithubPage {
    /// The rows this page needs, as an offset the database applies: asking
    /// for page 50 reads one page, not fifty.
    fn window(&self) -> PageWindow {
        PageWindow::new(self.per_page, (self.page - 1).saturating_mul(self.per_page))
    }

    fn convert<T, D: From<T>>(items: Vec<T>) -> Vec<D> {
        items.into_iter().map(D::from).collect()
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
        // Page-based paging stops at MAX_PAGED_ROWS, so a link past it would
        // advertise a page this gear answers with 422.
        let reachable_pages = MAX_PAGED_ROWS.checked_div(self.per_page).unwrap_or(0);
        let last_page = total
            .map(|total| total.div_ceil(self.per_page).max(1).min(reachable_pages))
            .filter(|_| reachable_pages > 0);
        let is_last_page =
            last_page.map_or(returned as u64 != self.per_page, |last| self.page >= last);

        let mut links = Vec::new();
        if !is_last_page && self.page < reachable_pages {
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

/// GitHub owner and repository names are ASCII letters, digits, `.`, `_`
/// and `-`. The path segments arrive percent-decoded, so anything else —
/// a `?`, `#`, `/`, a quote — could re-shape the URL or GraphQL query the
/// mirror sends to GitHub with its own token; such values are rejected
/// here, before any of them is used.
fn validate_repo_path(owner: &str, name: &str) -> Result<(), DomainError> {
    for (field, value) in [("owner", owner), ("name", name)] {
        let well_formed = !value.is_empty()
            && value != "."
            && value != ".."
            && value.len() <= 100
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
        if !well_formed {
            return Err(DomainError::Validation {
                field: field.to_owned(),
                message: "must be 1-100 characters from [A-Za-z0-9._-]".to_owned(),
            });
        }
    }
    Ok(())
}

pub async fn sync_repository(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<JsonBody<SyncSummaryDto>> {
    validate_repo_path(&owner, &name)?;
    let summary = svc.sync_repository(&ctx, &owner, &name).await?;
    Ok(Json(summary.into()))
}

pub async fn list_issues(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc
        .list_issues(&ctx, &owner, &name, page.window(), query.listing_filter()?)
        .await?;
    let items = items.items;
    let path = format!("/repos/{owner}/{name}/issues");
    Ok(respond_counted(
        &page,
        &path,
        GithubPage::convert(items),
        total,
    ))
}

pub async fn list_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommentDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_comments(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/comments");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_pull_requests(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<PullRequestDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc
        .list_pull_requests(&ctx, &owner, &name, page.window(), query.listing_filter()?)
        .await?;
    let items = items.items;
    let path = format!("/repos/{owner}/{name}/pulls");
    Ok(respond_counted(
        &page,
        &path,
        GithubPage::convert(items),
        total,
    ))
}

pub async fn list_reviews(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReviewDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_reviews(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/reviews");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_review_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReviewCommentDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_review_comments(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/comments");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_pull_request_files(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<PullRequestFileDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_pull_request_files(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/files");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_commits(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc.list_commits(&ctx, &owner, &name, page.window()).await?;
    let items = items.items;
    let path = format!("/repos/{owner}/{name}/commits");
    Ok(respond_counted(
        &page,
        &path,
        GithubPage::convert(items),
        total,
    ))
}

pub async fn list_branches(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<BranchDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_branches(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/branches");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_tags(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<TagDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_tags(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/tags");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_releases(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ReleaseDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_releases(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/releases");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_milestones(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<MilestoneDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_milestones(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/milestones");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_labels(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<LabelDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_labels(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/labels");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_contributors(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<ContributorDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_contributors(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/contributors");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_workflow_runs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<WorkflowRunsPageDto>)> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc
        .list_workflow_runs(&ctx, &owner, &name, page.window())
        .await?;
    let runs: Vec<WorkflowRunDto> = GithubPage::convert(items.items);
    let path = format!("/repos/{owner}/{name}/actions/runs");
    let headers = page.link_header_with_total(&path, runs.len(), Some(total));
    // GitHub's `total_count` spans every page, so it is a count, not the
    // length of the slice being served.
    let total_count = i64::try_from(total).unwrap_or(i64::MAX);
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
    validate_repo_path(&owner, &name)?;
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
    validate_repo_path(&owner, &name)?;
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
    validate_repo_path(&owner, &name)?;
    let repo = svc.get_repo(&ctx, &owner, &name).await?;
    Ok(Json(repo.into()))
}

pub async fn get_issue(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> ApiResult<JsonBody<IssueDto>> {
    validate_repo_path(&owner, &name)?;
    let issue = svc.get_issue(&ctx, &owner, &name, number).await?;
    Ok(Json(issue.into()))
}

pub async fn get_pull_request(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> ApiResult<JsonBody<PullRequestDto>> {
    validate_repo_path(&owner, &name)?;
    let pull = svc.get_pull_request(&ctx, &owner, &name, number).await?;
    Ok(Json(pull.into()))
}

pub async fn get_commit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
) -> ApiResult<JsonBody<CommitDto>> {
    validate_repo_path(&owner, &name)?;
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
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_commit_comments(&ctx, &owner, &name, &sha, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/commits/{sha}/comments");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_issue_events(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueEventDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_issue_events(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/events");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_issue_reactions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueReactionDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_issue_reactions(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/reactions");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_issue_timeline(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<IssueTimelineEventDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_issue_timeline(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/issues/{number}/timeline");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_deployments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<DeploymentDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_deployments(&ctx, &owner, &name, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/deployments");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_pull_request_commits(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_pull_request_commits(&ctx, &owner, &name, number, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/pulls/{number}/commits");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_commit_statuses(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> GithubList<CommitStatusDto> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let items = svc
        .list_commit_statuses(&ctx, &owner, &name, &sha, page.window())
        .await?
        .items;
    let path = format!("/repos/{owner}/{name}/commits/{sha}/statuses");
    Ok(respond(&page, &path, GithubPage::convert(items)))
}

pub async fn list_workflow_jobs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, run_id)): Path<(String, String, i64)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<WorkflowJobsPageDto>)> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc
        .list_workflow_jobs(&ctx, &owner, &name, run_id, page.window())
        .await?;
    let jobs: Vec<WorkflowJobDto> = GithubPage::convert(items.items);
    let path = format!("/repos/{owner}/{name}/actions/runs/{run_id}/jobs");
    let headers = page.link_header_with_total(&path, jobs.len(), Some(total));
    let total_count = i64::try_from(total).unwrap_or(i64::MAX);
    Ok((headers, Json(WorkflowJobsPageDto { total_count, jobs })))
}

pub async fn list_check_runs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    Query(query): Query<GithubPageQuery>,
) -> ApiResult<(HeaderMap, JsonBody<CheckRunsPageDto>)> {
    validate_repo_path(&owner, &name)?;
    let page = query.normalized()?;
    let (items, total) = svc
        .list_check_runs(&ctx, &owner, &name, &sha, page.window())
        .await?;
    let check_runs: Vec<CheckRunDto> = GithubPage::convert(items.items);
    let path = format!("/repos/{owner}/{name}/commits/{sha}/check-runs");
    let headers = page.link_header_with_total(&path, check_runs.len(), Some(total));
    let total_count = i64::try_from(total).unwrap_or(i64::MAX);
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
    let page = query.normalized()?;
    let items = svc.list_repos_page(&ctx, page.window()).await?;
    Ok(respond(&page, "/user/repos", GithubPage::convert(items)))
}

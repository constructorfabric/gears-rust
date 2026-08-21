use std::sync::Arc;

use axum::{Json, extract::Extension};
use toolkit::api::canonical_prelude::*;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;

use axum::extract::Path;

use crate::api::rest::routes::ConcreteService;

use super::dto::{
    BranchDto, CommentDto, CommitDto, ContributorDto, GithubMirrorHealthDto, IssueDto, LabelDto,
    MilestoneDto, PullRequestDto, ReleaseDto, RepositoryDto, ReviewCommentDto, ReviewDto,
    SyncSummaryDto,
};

pub async fn health(
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<GithubMirrorHealthDto>> {
    let status = svc.status();
    Ok(Json(status.into()))
}

pub async fn list_repositories(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    OData(query): OData,
) -> ApiResult<JsonPage<RepositoryDto>> {
    let page: Page<_> = svc.list_repositories(&ctx, &query).await?;
    Ok(Json(page.map_items(RepositoryDto::from)))
}

pub async fn list_issues(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<IssueDto>> {
    let page: Page<_> = svc.list_issues(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(IssueDto::from)))
}

pub async fn list_pull_requests(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<PullRequestDto>> {
    let page: Page<_> = svc.list_pull_requests(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(PullRequestDto::from)))
}

pub async fn list_commits(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<CommitDto>> {
    let page: Page<_> = svc.list_commits(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(CommitDto::from)))
}

pub async fn sync_repository(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<JsonBody<SyncSummaryDto>> {
    let summary = svc.sync_repository(&ctx, &owner, &name).await?;
    Ok(Json(summary.into()))
}

pub async fn list_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    OData(query): OData,
) -> ApiResult<JsonPage<CommentDto>> {
    let page: Page<_> = svc
        .list_comments(&ctx, &owner, &name, number, &query)
        .await?;
    Ok(Json(page.map_items(CommentDto::from)))
}

pub async fn list_review_comments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    OData(query): OData,
) -> ApiResult<JsonPage<ReviewCommentDto>> {
    let page: Page<_> = svc
        .list_review_comments(&ctx, &owner, &name, number, &query)
        .await?;
    Ok(Json(page.map_items(ReviewCommentDto::from)))
}

pub async fn list_reviews(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    OData(query): OData,
) -> ApiResult<JsonPage<ReviewDto>> {
    let page: Page<_> = svc
        .list_reviews(&ctx, &owner, &name, number, &query)
        .await?;
    Ok(Json(page.map_items(ReviewDto::from)))
}

pub async fn list_labels(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<LabelDto>> {
    let page: Page<_> = svc.list_labels(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(LabelDto::from)))
}

pub async fn list_milestones(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<MilestoneDto>> {
    let page: Page<_> = svc.list_milestones(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(MilestoneDto::from)))
}

pub async fn list_releases(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<ReleaseDto>> {
    let page: Page<_> = svc.list_releases(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(ReleaseDto::from)))
}

pub async fn list_branches(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<BranchDto>> {
    let page: Page<_> = svc.list_branches(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(BranchDto::from)))
}

pub async fn list_contributors(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path((owner, name)): Path<(String, String)>,
    OData(query): OData,
) -> ApiResult<JsonPage<ContributorDto>> {
    let page: Page<_> = svc.list_contributors(&ctx, &owner, &name, &query).await?;
    Ok(Json(page.map_items(ContributorDto::from)))
}

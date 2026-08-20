use std::sync::Arc;

use axum::{Json, extract::Extension};
use toolkit::api::canonical_prelude::*;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;

use axum::extract::Path;

use crate::api::rest::routes::ConcreteService;

use super::dto::{
    CommitDto, GithubMirrorHealthDto, IssueDto, PullRequestDto, RepositoryDto, SyncSummaryDto,
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

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{PageWindow, RepoRecord, RepoRepository};
use github_mirror::infra::github::cache::{CacheKey, CachedResponse, HttpCache};
use github_mirror::infra::github::client::GithubClient;
use github_mirror::infra::github::compression::Compression;
use github_mirror::infra::storage::sea_orm_repo::{SeaOrmHttpCache, SeaOrmRepoRepository};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

const API: &str = "https://api.github.com";

fn router_for(service: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service).layer(axum::Extension(ctx))
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn send(router: Router, method: Method, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(request).await.unwrap()
}

fn entry(url: &str) -> CachedResponse {
    CachedResponse {
        body: format!(r#"{{"url":"{url}"}}"#),
        etag: None,
        last_modified: None,
        next_page: None,
    }
}

fn key(url: &str) -> CacheKey {
    CacheKey::compute("GET", url, "application/json")
}

async fn cached(cache: &SeaOrmHttpCache, scope: &AccessScope, url: &str) -> bool {
    cache.get(scope, &key(url)).await.unwrap().is_some()
}

#[tokio::test]
async fn clearing_one_repository_leaves_its_neighbours_alone() {
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db.clone()));
    let cache = SeaOrmHttpCache::new(Arc::clone(&provider), Compression::None);
    let urls = [
        format!("{API}/repos/acme/widget/issues?per_page=100"),
        format!("{API}/repos/acme/widget-fork/issues?per_page=100"),
        format!("{API}/repos/other/thing"),
    ];
    for url in &urls {
        cache
            .put(&scope, tenant, &key(url), url, entry(url))
            .await
            .unwrap();
    }
    let github = GithubClient::with_cache(
        API.to_owned(),
        None,
        Arc::new(SeaOrmHttpCache::new(provider, Compression::None)),
    )
    .unwrap();
    let service = common::service_with_github(db, API, Arc::new(github));
    let router = router_for(service, common::caller_in(tenant));

    let response = send(
        router.clone(),
        Method::DELETE,
        "/github-mirror/v1/cache?repo=acme/widget",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["scope"], "acme/widget");
    assert_eq!(body["entries_removed"], 1);
    assert!(!cached(&cache, &scope, &urls[0]).await);
    assert!(
        cached(&cache, &scope, &urls[1]).await,
        "widget-fork is not below widget/"
    );
    assert!(cached(&cache, &scope, &urls[2]).await);

    let response = send(router, Method::DELETE, "/github-mirror/v1/cache?owner=acme").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["entries_removed"], 1);
    assert!(!cached(&cache, &scope, &urls[1]).await);
    assert!(
        cached(&cache, &scope, &urls[2]).await,
        "another owner's entries stay"
    );
}

fn repo_record(id: i64, owner: &str) -> RepoRecord {
    RepoRecord {
        node_id: None,
        id,
        owner: owner.to_owned(),
        name: format!("repo-{id}"),
        full_name: format!("{owner}/repo-{id}"),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: None,
        clone_url: None,
    }
}

#[tokio::test]
async fn every_repository_of_an_owner_with_more_than_a_page_is_found() {
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let db = common::inmem_db().await;
    let repos = SeaOrmRepoRepository::new(Arc::new(DBProvider::<DbError>::new(db)));
    let page = i64::try_from(PageWindow::MAX_LIMIT).unwrap();
    repos
        .upsert(&scope, tenant, repo_record(page + 2, "other"))
        .await
        .unwrap();
    for id in 1..=page + 1 {
        repos
            .upsert(&scope, tenant, repo_record(id, "acme"))
            .await
            .unwrap();
    }

    let mut ids = repos.ids_by_owner(&scope, "acme").await.unwrap();
    ids.sort_unstable();
    assert_eq!(ids, (1..=page + 1).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_clear_needs_an_owner_or_a_full_slug() {
    let service = common::service_over(common::inmem_db().await, API);
    let router = router_for(service, common::caller());

    for uri in [
        "/github-mirror/v1/cache",
        "/github-mirror/v1/cache?repo=widget",
    ] {
        let response = send(router.clone(), Method::DELETE, uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}

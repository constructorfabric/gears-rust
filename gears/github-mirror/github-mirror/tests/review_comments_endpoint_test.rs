#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{RepoRecord, ReviewCommentRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 950,
        owner: "acme".to_owned(),
        name: "widget".to_owned(),
        full_name: "acme/widget".to_owned(),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: None,
        clone_url: None,
    }
}

fn review_comment_record(id: i64, pull_number: i64, created_at: &str) -> ReviewCommentRecord {
    ReviewCommentRecord {
        id,
        repo_id: 0,
        pull_number,
        author_login: Some("dave".to_owned()),
        body: Some("rename this".to_owned()),
        path: Some("src/lib.rs".to_owned()),
        diff_hunk: Some("@@ -1 +1 @@".to_owned()),
        in_reply_to_id: None,
        commit_id: Some("h1".to_owned()),
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
        html_url: None,
        position: Some(5),
        original_position: Some(3),
        pull_request_review_id: Some(31),
        line: Some(12),
        original_line: Some(12),
        start_line: None,
        original_start_line: None,
        side: Some("RIGHT".to_owned()),
        start_side: None,
        subject_type: Some("line".to_owned()),
    }
}

fn router_for(service: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service).layer(axum::Extension(ctx))
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get(router: Router, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.oneshot(request).await.unwrap()
}

#[tokio::test]
async fn review_comments_are_listed_oldest_first_for_their_pull() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_comment(
            &ctx,
            "acme",
            "widget",
            review_comment_record(2, 7, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("review comment seed must succeed");
    service
        .upsert_review_comment(
            &ctx,
            "acme",
            "widget",
            review_comment_record(1, 7, "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("review comment seed must succeed");
    service
        .upsert_review_comment(
            &ctx,
            "acme",
            "widget",
            review_comment_record(3, 8, "2026-08-19T00:00:00Z"),
        )
        .await
        .expect("review comment seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/comments").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only pull 7 comments must be returned");
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[1]["id"], 2);
    assert_eq!(items[0]["path"], "src/lib.rs");
    assert_eq!(
        items[0]["position"], 5,
        "diff-anchoring fields must round-trip through the GitHub-compatible surface"
    );
    assert_eq!(items[0]["original_position"], 3);
}

#[tokio::test]
async fn review_comments_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/pulls/7/comments").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn review_comments_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_comment(
            &owner,
            "acme",
            "widget",
            review_comment_record(4, 7, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("review comment seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/pulls/7/comments").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn review_comment_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_comment(
            &ctx,
            "acme",
            "widget",
            review_comment_record(5, 7, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = review_comment_record(5, 7, "2026-08-20T00:00:00Z");
    updated.body = Some("amended".to_owned());
    service
        .upsert_review_comment(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/comments").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["body"], "amended");
}

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::RepoRecord;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record(id: i64, name: &str, clone_url: Option<&str>) -> RepoRecord {
    RepoRecord {
        node_id: None,
        id,
        owner: "acme".to_owned(),
        name: name.to_owned(),
        full_name: format!("acme/{name}"),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: None,
        clone_url: clone_url.map(ToOwned::to_owned),
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
async fn the_mirror_answers_who_it_is() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/user").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["login"], "github-mirror");
    assert_eq!(json["type"], "Bot");
    assert!(json["name"].is_string());
}

#[tokio::test]
async fn user_repos_lists_the_tenants_mirrored_repositories() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(
            &ctx,
            repo_record(1, "widget", Some("https://github.com/acme/widget.git")),
        )
        .await
        .expect("repo seed must succeed");
    service
        .upsert_repo(&ctx, repo_record(2, "gadget", None))
        .await
        .expect("repo seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/user/repos").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2);

    let widget = items
        .iter()
        .find(|r| r["name"] == "widget")
        .expect("widget must be listed");
    assert_eq!(widget["clone_url"], "https://github.com/acme/widget.git");
    assert_eq!(widget["full_name"], "acme/widget");
    assert_eq!(widget["owner"]["login"], "acme");

    let gadget = items
        .iter()
        .find(|r| r["name"] == "gadget")
        .expect("gadget must be listed");
    assert_eq!(
        gadget["clone_url"], "https://github.com/acme/gadget.git",
        "a row stored without a clone URL must still serve a usable one"
    );
}

#[tokio::test]
async fn user_repos_is_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record(3, "secret", None))
        .await
        .expect("repo seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let json = body_json(get(router, "/user/repos").await).await;

    assert_eq!(
        json.as_array().expect("items").len(),
        0,
        "another tenant must see none of these repositories"
    );
}

#[tokio::test]
async fn user_repos_paginates_github_style() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    for id in 10..14 {
        service
            .upsert_repo(&ctx, repo_record(id, &format!("repo{id}"), None))
            .await
            .expect("repo seed must succeed");
    }

    let router = router_for(service, ctx);
    let json = body_json(get(router, "/user/repos?per_page=2&page=2").await).await;
    let items = json.as_array().expect("items");
    assert_eq!(
        items.len(),
        2,
        "the second page must hold the next two rows"
    );
}

#[tokio::test]
async fn the_last_page_advertises_itself_in_the_link_header() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    for id in 20..23 {
        service
            .upsert_repo(&ctx, repo_record(id, &format!("repo{id}"), None))
            .await
            .expect("repo seed must succeed");
    }

    let router = router_for(service, ctx);

    // Three rows, two per page: page 1 is full, so where the listing ends is
    // not yet known and only `next` is offered.
    let first = get(router.clone(), "/user/repos?per_page=2&page=1").await;
    let links = first
        .headers()
        .get(axum::http::header::LINK)
        .expect("a full page must link onward")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(links.contains(r#"rel="next""#), "{links}");
    assert!(
        !links.contains(r#"rel="last""#),
        "a full page cannot know the last page without counting: {links}"
    );

    // Page 2 holds the single remaining row, which makes it the last page.
    let second = get(router, "/user/repos?per_page=2&page=2").await;
    let links = second
        .headers()
        .get(axum::http::header::LINK)
        .expect("a later page must link back")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(links.contains(r#"rel="prev""#), "{links}");
    assert!(links.contains(r#"rel="first""#), "{links}");
    assert!(
        links.contains(r#"</user/repos?page=2&per_page=2>; rel="last""#),
        "the short page is the last one: {links}"
    );
    assert!(!links.contains(r#"rel="next""#), "{links}");
}

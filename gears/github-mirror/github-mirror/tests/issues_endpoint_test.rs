#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{IssueRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 900,
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

fn issue_record(id: i64, number: i64, title: &str) -> IssueRecord {
    IssueRecord {
        author_login: Some("alice".to_owned()),
        author_json: None,
        assignees_json: None,
        labels_json: None,
        comments_count: None,
        locked: None,
        node_id: None,
        id,
        repo_id: 0,
        number,
        title: title.to_owned(),
        body: Some("text".to_owned()),
        state: "open".to_owned(),
        is_pull_request: false,
        created_at: "2026-08-20T00:00:00Z".to_owned(),
        updated_at: "2026-08-20T00:00:00Z".to_owned(),
        closed_at: None,
        html_url: None,
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
async fn issues_are_listed_for_a_mirrored_repository() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue(&ctx, "acme", "widget", issue_record(1, 11, "first"))
        .await
        .expect("issue seed must succeed");
    service
        .upsert_issue(&ctx, "acme", "widget", issue_record(2, 12, "second"))
        .await
        .expect("issue seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/issues").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["number"], 11);
    assert_eq!(items[0]["title"], "first");
}

#[tokio::test]
async fn issues_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/issues").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn issues_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue(&owner, "acme", "widget", issue_record(1, 11, "secret"))
        .await
        .expect("issue seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/issues").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn issues_default_to_open_like_github() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");

    let mut open = issue_record(1, 11, "still open");
    open.state = "open".to_owned();
    let mut closed = issue_record(2, 12, "done");
    closed.state = "closed".to_owned();
    for record in [open, closed] {
        service
            .upsert_issue(&ctx, "acme", "widget", record)
            .await
            .expect("issue seed must succeed");
    }

    let router = router_for(service, ctx);

    let default = body_json(get(router.clone(), "/repos/acme/widget/issues").await).await;
    let items = default.as_array().expect("items");
    assert_eq!(items.len(), 1, "GitHub's default filter is state=open");
    assert_eq!(items[0]["number"], 11);

    let closed =
        body_json(get(router.clone(), "/repos/acme/widget/issues?state=closed").await).await;
    let items = closed.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["number"], 12);

    let all = body_json(get(router, "/repos/acme/widget/issues?state=all").await).await;
    assert_eq!(all.as_array().expect("items").len(), 2);
}

#[tokio::test]
async fn errors_on_the_compatible_surface_use_githubs_shape() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    let router = router_for(service, ctx);

    let response = get(router, "/repos/acme/nope/issues").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json"),
        "GitHub answers errors as application/json, not problem+json"
    );

    let json = body_json(response).await;
    assert!(
        json["message"].is_string(),
        "a client reads `message` out of GitHub's error body: {json:?}"
    );
    assert!(json["documentation_url"].is_string(), "{json:?}");
}

#[tokio::test]
async fn issues_carry_the_fields_a_github_client_renders() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");

    let mut record = issue_record(9, 42, "needs review");
    record.author_login = Some("alice".to_owned());
    record.author_json = Some(
        r#"{"id":71,"login":"alice","type":"User","avatar_url":"https://avatars.githubusercontent.com/u/71","html_url":"https://github.com/alice","site_admin":false}"#
            .to_owned(),
    );
    record.assignees_json =
        Some(r#"[{"id":72,"login":"bob"},{"id":73,"login":"carol"}]"#.to_owned());
    record.labels_json =
        Some(r#"[{"id":1,"name":"bug","color":"d73a4a"},{"id":2,"name":"p1"}]"#.to_owned());
    record.comments_count = Some(7);
    record.locked = Some(false);
    service
        .upsert_issue(&ctx, "acme", "widget", record)
        .await
        .expect("issue seed must succeed");

    let router = router_for(service, ctx);
    let json = body_json(get(router, "/repos/acme/widget/issues").await).await;
    let issue = &json.as_array().expect("items")[0];

    assert_eq!(issue["user"]["login"], "alice");
    assert_eq!(
        issue["user"]["id"], 71,
        "the whole GitHub user object is mirrored"
    );
    assert_eq!(issue["user"]["type"], "User");
    assert_eq!(
        issue["user"]["avatar_url"],
        "https://avatars.githubusercontent.com/u/71"
    );
    assert_eq!(issue["user"]["site_admin"], false);
    assert_eq!(
        issue["assignees"],
        serde_json::json!([
            {"login": "bob", "id": 72},
            {"login": "carol", "id": 73}
        ]),
        "ids ride along so a consumer can join gm_contributors"
    );
    assert_eq!(
        issue["labels"],
        serde_json::json!([
            {"id": 1, "name": "bug", "color": "d73a4a"},
            {"id": 2, "name": "p1"}
        ])
    );
    assert_eq!(issue["comments"], 7);
    assert_eq!(issue["locked"], false);
}

#[tokio::test]
async fn a_full_page_names_the_last_page_from_the_total() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    for number in 1..=5 {
        service
            .upsert_issue(
                &ctx,
                "acme",
                "widget",
                issue_record(number, number, "open one"),
            )
            .await
            .expect("issue seed must succeed");
    }

    let router = router_for(service, ctx);

    // Five issues, two per page: page 1 is full, and the count says where the
    // listing ends — GitHub always answers with `last`, so the mirror does too.
    let response = get(
        router.clone(),
        "/repos/acme/widget/issues?per_page=2&page=1",
    )
    .await;
    let links = response
        .headers()
        .get(axum::http::header::LINK)
        .expect("a paginated listing must link")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(links.contains(r#"rel="next""#), "{links}");
    assert!(
        links.contains(r#"page=3&per_page=2>; rel="last""#),
        "five rows at two a page end on page 3: {links}"
    );

    // The last page offers no `next`, and still names itself.
    let response = get(router, "/repos/acme/widget/issues?per_page=2&page=3").await;
    let links = response
        .headers()
        .get(axum::http::header::LINK)
        .expect("the last page still links back")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!links.contains(r#"rel="next""#), "{links}");
    assert!(
        links.contains(r#"page=3&per_page=2>; rel="last""#),
        "{links}"
    );
}

//! Conditional GET on the plan list, the plan counts and the book list (D-518).
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use axum::{
    Router,
    body::Body,
    http::{Request, header},
};
use plan_support::{book, plan, setup};
use toolkit_security::SecurityContext;
use tower::ServiceExt;

struct Answer {
    status: u16,
    etag: String,
    cache_control: String,
    body: Vec<u8>,
}

async fn get(app: &Router, ctx: &SecurityContext, path: &str, tag: Option<&str>) -> Answer {
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("/bss-pricing/v1{path}"))
        .extension(ctx.clone());
    if let Some(tag) = tag {
        request = request.header(header::IF_NONE_MATCH, tag);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Answer {
        status,
        etag,
        cache_control,
        body,
    }
}

async fn revalidates(app: &Router, ctx: &SecurityContext, path: &str) -> String {
    let first = get(app, ctx, path, None).await;
    assert_eq!(
        first.status,
        200,
        "{path}: {}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(first.cache_control, "private, no-cache", "{path}");
    assert!(
        first.etag.starts_with("W/\""),
        "{path}: the tag is the body, got {}",
        first.etag
    );
    assert!(!first.body.is_empty(), "{path}");
    let again = get(app, ctx, path, Some(&first.etag)).await;
    assert_eq!(again.status, 304, "{path}");
    assert!(again.body.is_empty(), "{path}");
    assert_eq!(again.etag, first.etag, "{path}");
    assert_eq!(again.cache_control, "private, no-cache", "{path}");
    let star = get(app, ctx, path, Some("*")).await;
    assert_eq!(star.status, 304, "{path}: *");
    first.etag
}

#[tokio::test]
async fn the_plan_list_and_its_counts_answer_304() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    let mut before = Vec::new();
    for path in ["/plans", "/plans/counts"] {
        before.push(revalidates(&f.app, &f.ctx, path).await);
    }
    plan(&f, "pro", eur).await;
    for (path, old) in ["/plans", "/plans/counts"].into_iter().zip(before) {
        let fresh = get(&f.app, &f.ctx, path, Some(&old)).await;
        assert_eq!(fresh.status, 200, "{path}");
        assert_ne!(fresh.etag, old, "{path}");
        assert!(fresh.etag.starts_with("W/\""), "{path}: {}", fresh.etag);
        assert_eq!(fresh.cache_control, "private, no-cache", "{path}");
    }
}

#[tokio::test]
async fn the_book_list_answers_304_and_a_new_book_changes_the_tag() {
    let (f, _) = setup().await;
    let old = revalidates(&f.app, &f.ctx, "/price-books").await;
    book(&f, "eur").await;
    let fresh = get(&f.app, &f.ctx, "/price-books", Some(&old)).await;
    assert_eq!(
        fresh.status,
        200,
        "{}",
        String::from_utf8_lossy(&fresh.body)
    );
    assert_ne!(fresh.etag, old);
    assert_eq!(fresh.cache_control, "private, no-cache");
}

/// `GET /settings` is one document: its strong `ETag` stays the row version a `PUT` sends back as
/// `If-Match`, and `If-None-Match` matches that same tag by weak comparison (D-518).
#[tokio::test]
async fn the_settings_answer_304_on_their_strong_version_tag() {
    let (f, _) = setup().await;
    let first = get(&f.app, &f.ctx, "/settings", None).await;
    assert_eq!(
        first.status,
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(first.etag, "\"0\"", "the strong version tag is unchanged");
    assert_eq!(first.cache_control, "private, no-cache");
    for tag in ["\"0\"", "W/\"0\"", "\"9\", \"0\"", "*"] {
        let again = get(&f.app, &f.ctx, "/settings", Some(tag)).await;
        assert_eq!(again.status, 304, "{tag}");
        assert!(again.body.is_empty(), "{tag}");
        assert_eq!(again.etag, "\"0\"", "{tag}");
        assert_eq!(again.cache_control, "private, no-cache", "{tag}");
    }
    let other = get(&f.app, &f.ctx, "/settings", Some("\"1\"")).await;
    assert_eq!(other.status, 200, "an unrelated tag reads the body");
    assert!(!other.body.is_empty());
    let mut body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    for field in ["version", "updated_at", "updated_by"] {
        body.as_object_mut().unwrap().remove(field);
    }
    body["currencies"] = serde_json::json!(["EUR"]);
    let (s, b, tag) = f.call("PUT", "/settings", body, Some("\"0\""), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(tag, "\"1\"");
    let fresh = get(&f.app, &f.ctx, "/settings", Some(&first.etag)).await;
    assert_eq!(fresh.status, 200, "the old tag no longer matches");
    assert_eq!(fresh.etag, "\"1\"");
    assert_eq!(fresh.cache_control, "private, no-cache");
    let now = get(&f.app, &f.ctx, "/settings", Some("\"1\"")).await;
    assert_eq!(now.status, 304);
    assert_eq!(now.etag, "\"1\"");
}

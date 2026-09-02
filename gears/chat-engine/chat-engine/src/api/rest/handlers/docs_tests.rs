//! Tests for the documentation handlers.

use axum::body::to_bytes;
use axum::http::Uri;

use crate::api::rest::docs::TeeRegistry;
use toolkit::api::{OpenApiRegistry, OperationBuilder, OperationSpec};
use utoipa::openapi::{RefOr, schema::Schema};

use super::*;

/// Host registry stand-in: the tee forwards to it, nothing here inspects it.
struct NoopHost;

impl OpenApiRegistry for NoopHost {
    fn register_operation(&self, _spec: &OperationSpec) {}

    fn ensure_schema_raw(&self, root_name: &str, _schemas: Vec<(String, RefOr<Schema>)>) -> String {
        root_name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A document holding one registered operation.
fn built_doc() -> Arc<GearOpenApiDoc> {
    let host = NoopHost;
    let tee = TeeRegistry::new(&host);
    tee.register_operation(
        OperationBuilder::<_, _, ()>::new(http::Method::GET, "/chat-engine/v1/sessions").spec(),
    );

    let doc = Arc::new(GearOpenApiDoc::default());
    doc.build(&tee);
    doc
}

async fn body_string(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

#[tokio::test]
async fn openapi_json_serves_the_document() {
    let uri: Uri = "/chat-engine/v1/openapi".parse().expect("uri");
    let response = openapi_json(OriginalUri(uri), Extension(built_doc())).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).expect("type"),
        "application/json"
    );
    // The document is rebuilt on every deploy; never let a proxy pin an old one.
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("cache"),
        "no-store"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("valid JSON");
    assert_eq!(parsed["info"]["title"], "Chat Engine API");
    assert!(parsed["paths"]["/chat-engine/v1/sessions"].is_object());
}

#[tokio::test]
async fn openapi_json_reflects_the_gateway_prefix_in_servers() {
    // One shared handle, as the Extension layer hands out: every request must
    // be told the base URL it actually arrived on, not the first one's.
    let doc = built_doc();

    for (path, expected) in [
        ("/cf/chat-engine/v1/openapi", Some("/cf")),
        ("/other/chat-engine/v1/openapi", Some("/other")),
        ("/chat-engine/v1/openapi", None),
        // Back to the first prefix: no state may have been settled meanwhile.
        ("/cf/chat-engine/v1/openapi", Some("/cf")),
    ] {
        let uri: Uri = path.parse().expect("uri");
        let response = openapi_json(OriginalUri(uri), Extension(Arc::clone(&doc))).await;
        let parsed: serde_json::Value =
            serde_json::from_str(&body_string(response).await).expect("valid JSON");

        match expected {
            Some(url) => assert_eq!(
                parsed["servers"],
                serde_json::json!([{ "url": url }]),
                "{path} should advertise {url} as its base URL"
            ),
            None => assert!(
                parsed.get("servers").is_none(),
                "{path} is mounted at the root and needs no servers entry"
            ),
        }
    }
}

#[tokio::test]
async fn openapi_json_reports_503_when_the_document_was_never_built() {
    let uri: Uri = "/chat-engine/v1/openapi".parse().expect("uri");
    let response = openapi_json(
        OriginalUri(uri),
        Extension(Arc::new(GearOpenApiDoc::default())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(body_string(response).await.contains("unavailable"));
}

#[tokio::test]
async fn docs_page_serves_the_reference_html() {
    let Html(page) = docs_page().await;

    assert!(page.starts_with("<!DOCTYPE html>"));
    assert!(page.contains("<elements-api"));
}

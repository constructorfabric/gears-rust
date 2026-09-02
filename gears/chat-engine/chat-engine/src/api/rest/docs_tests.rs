//! Tests for the gear-scoped OpenAPI document.

use std::sync::Mutex;

use http::Method;
use toolkit::api::OperationBuilder;

use super::*;

/// Host registry stand-in that records what the tee forwards to it.
#[derive(Default)]
struct RecordingHost {
    operations: Mutex<Vec<String>>,
    schemas: Mutex<Vec<String>>,
}

impl OpenApiRegistry for RecordingHost {
    fn register_operation(&self, spec: &OperationSpec) {
        self.operations
            .lock()
            .expect("host operations lock")
            .push(format!("{}:{}", spec.method, spec.path));
    }

    fn ensure_schema_raw(&self, root_name: &str, _schemas: Vec<(String, RefOr<Schema>)>) -> String {
        self.schemas
            .lock()
            .expect("host schemas lock")
            .push(root_name.to_owned());
        root_name.to_owned()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn spec(path: &str) -> OperationSpec {
    OperationBuilder::<_, _, ()>::new(Method::GET, path)
        .spec()
        .clone()
}

/// Minimal document with one path, enough to exercise `render`.
fn document() -> Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "paths": { "/chat-engine/v1/sessions": { "post": { "responses": {} } } }
    })
}

fn built_doc() -> GearOpenApiDoc {
    let doc = GearOpenApiDoc::default();
    doc.document.set(document()).expect("fresh document");
    doc
}

#[test]
fn tee_forwards_operations_to_the_host() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);

    tee.register_operation(&spec("/chat-engine/v1/sessions"));

    assert_eq!(
        *host.operations.lock().expect("host operations lock"),
        ["GET:/chat-engine/v1/sessions"]
    );
}

#[test]
fn tee_forwards_schemas_to_the_host_and_returns_its_name() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);

    let name = tee.ensure_schema_raw("SessionDto", Vec::new());

    assert_eq!(name, "SessionDto");
    assert_eq!(
        *host.schemas.lock().expect("host schemas lock"),
        ["SessionDto"]
    );
}

#[test]
fn tee_keeps_a_private_copy_of_what_passed_through() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);

    tee.register_operation(&spec("/chat-engine/v1/sessions"));
    tee.register_operation(&spec("/chat-engine/v1/messages/{id}"));

    let recorded: Vec<String> = tee
        .gear_registry()
        .operation_specs
        .iter()
        .map(|entry| entry.value().path.clone())
        .collect();

    assert_eq!(recorded.len(), 2);
    assert!(recorded.contains(&"/chat-engine/v1/sessions".to_owned()));
    assert!(recorded.contains(&"/chat-engine/v1/messages/{id}".to_owned()));
}

#[test]
fn tee_private_copy_excludes_what_other_gears_registered() {
    // The host's own surface is invisible to the tee: only calls routed
    // *through* the decorator land in the private registry.
    let host = RecordingHost::default();
    host.register_operation(&spec("/credstore/v1/secrets"));

    let tee = TeeRegistry::new(&host);
    tee.register_operation(&spec("/chat-engine/v1/sessions"));

    let recorded: Vec<String> = tee
        .gear_registry()
        .operation_specs
        .iter()
        .map(|entry| entry.value().path.clone())
        .collect();

    assert_eq!(recorded, ["/chat-engine/v1/sessions"]);
}

#[test]
fn build_produces_a_document_covering_the_teed_operations() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);
    tee.register_operation(&spec("/chat-engine/v1/sessions"));

    let doc = GearOpenApiDoc::default();
    doc.build(&tee);

    let rendered = doc.render("/chat-engine/v1/openapi").expect("rendered");
    let parsed: Value = serde_json::from_str(&rendered).expect("valid JSON");

    assert_eq!(parsed["info"]["title"], "Chat Engine API");
    assert!(parsed["paths"]["/chat-engine/v1/sessions"].is_object());
}

#[test]
fn mount_prefix_is_whatever_precedes_the_gear_base_path() {
    assert_eq!(mount_prefix("/cf/chat-engine/v1/openapi"), "/cf");
    assert_eq!(mount_prefix("/chat-engine/v1/openapi"), "");
    assert_eq!(mount_prefix("/a/b/chat-engine/v1/docs"), "/a/b");
}

#[test]
fn mount_prefix_falls_back_to_empty_for_unexpected_paths() {
    assert_eq!(mount_prefix("/somewhere/else"), "");
}

#[test]
fn render_reports_unavailable_until_built() {
    let doc = GearOpenApiDoc::default();

    assert!(doc.render("/cf/chat-engine/v1/openapi").is_none());
}

#[test]
fn render_injects_the_mount_prefix_as_the_server() {
    let doc = built_doc();

    let rendered = doc.render("/cf/chat-engine/v1/openapi").expect("rendered");
    let parsed: Value = serde_json::from_str(&rendered).expect("valid JSON");

    assert_eq!(parsed["servers"], serde_json::json!([{ "url": "/cf" }]));
}

#[test]
fn render_omits_servers_when_mounted_at_the_root() {
    let doc = built_doc();

    let rendered = doc.render("/chat-engine/v1/openapi").expect("rendered");
    let parsed: Value = serde_json::from_str(&rendered).expect("valid JSON");

    assert!(parsed.get("servers").is_none());
}

#[test]
fn docs_page_points_at_the_sibling_document() {
    // Relative, so the page works under any gateway `prefix_path`.
    assert!(DOCS_PAGE.contains(r#"apiDescriptionUrl="./openapi""#));
}

#[test]
fn tee_downcasts_to_the_host_it_stands_in_for() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);

    // A transparent decorator: `as_any` must reach past the wrapper, or a
    // caller downcasting the registry silently gets `None`.
    assert!(tee.as_any().downcast_ref::<RecordingHost>().is_some());
    assert!(tee.as_any().downcast_ref::<TeeRegistry<'_>>().is_none());
}

#[test]
fn build_is_idempotent_and_keeps_the_first_document() {
    let host = RecordingHost::default();
    let tee = TeeRegistry::new(&host);
    tee.register_operation(&spec("/chat-engine/v1/sessions"));

    let doc = GearOpenApiDoc::default();
    doc.build(&tee);

    // A second operation registered after the snapshot must not appear: the
    // repeated build is a no-op, not a refresh.
    tee.register_operation(&spec("/chat-engine/v1/messages/{id}"));
    doc.build(&tee);

    let parsed: Value =
        serde_json::from_str(&doc.render("/chat-engine/v1/openapi").expect("rendered"))
            .expect("valid JSON");
    assert!(parsed["paths"]["/chat-engine/v1/sessions"].is_object());
    assert!(parsed["paths"]["/chat-engine/v1/messages/{id}"].is_null());
}

#[test]
fn render_resolves_servers_per_call_not_per_process() {
    let doc = built_doc();

    let under_cf = doc.render("/cf/chat-engine/v1/openapi").expect("rendered");
    let under_other = doc
        .render("/other/chat-engine/v1/openapi")
        .expect("rendered");
    let at_root = doc.render("/chat-engine/v1/openapi").expect("rendered");

    // Each caller must be told its own base URL. Caching one rendering for the
    // process would hand the second and third callers the first one's.
    let parse = |s: &str| -> Value { serde_json::from_str(s).expect("valid JSON") };
    assert_eq!(
        parse(&under_cf)["servers"],
        serde_json::json!([{ "url": "/cf" }])
    );
    assert_eq!(
        parse(&under_other)["servers"],
        serde_json::json!([{ "url": "/other" }])
    );
    assert!(parse(&at_root).get("servers").is_none());
}

/// Drive the mounted routes through a real router, so the registration itself
/// is exercised rather than just the handlers behind it.
mod mount {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    fn router() -> Router {
        let host = RecordingHost::default();
        let tee = TeeRegistry::new(&host);
        // A real operation so the served document is not merely empty.
        tee.register_operation(&spec("/chat-engine/v1/sessions"));
        crate::api::rest::docs::mount(Router::new(), &tee)
    }

    async fn get(path: &str) -> (StatusCode, String, String) {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status();
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap_or_default().to_owned())
            .unwrap_or_default();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            content_type,
            String::from_utf8(body.to_vec()).expect("utf-8"),
        )
    }

    #[tokio::test]
    async fn serves_the_reference_page() {
        let (status, content_type, body) = get("/chat-engine/v1/docs").await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        assert!(body.contains("<elements-api"));
    }

    #[tokio::test]
    async fn serves_the_document() {
        let (status, content_type, body) = get("/chat-engine/v1/openapi").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/json");

        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(parsed["info"]["title"], "Chat Engine API");
        // The route the fixture registered, plus the two mount() registers.
        assert!(parsed["paths"]["/chat-engine/v1/sessions"].is_object());
        assert!(parsed["paths"]["/chat-engine/v1/docs"].is_object());
        assert!(parsed["paths"]["/chat-engine/v1/openapi"].is_object());
    }

    #[tokio::test]
    async fn documents_itself_as_anonymous() {
        let (_, _, body) = get("/chat-engine/v1/openapi").await;
        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");

        // No `security` block: a reference behind a token cannot be read by
        // whoever needs it most.
        for path in ["/chat-engine/v1/docs", "/chat-engine/v1/openapi"] {
            assert!(
                parsed["paths"][path]["get"].get("security").is_none(),
                "{path} should be anonymous"
            );
        }
    }
}

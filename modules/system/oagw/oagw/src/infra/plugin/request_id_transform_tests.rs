use std::collections::HashMap;

use modkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::domain::plugin::{TransformPlugin, TransformResponseContext};

fn test_security_context() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

fn make_request_ctx(headers: Vec<(String, String)>) -> TransformRequestContext {
    TransformRequestContext {
        method: "POST".to_string(),
        path: "/v1/test".to_string(),
        query: Vec::new(),
        headers,
        config: HashMap::new(),
        security_context: test_security_context(),
    }
}

#[tokio::test]
async fn injects_request_id_when_missing() {
    let plugin = RequestIdTransformPlugin;
    let mut ctx = make_request_ctx(Vec::new());
    plugin.on_request(&mut ctx).await.unwrap();

    let id = ctx
        .headers
        .iter()
        .find(|(k, _)| k == "x-request-id")
        .map(|(_, v)| v.as_str())
        .expect("should inject header");
    // Verify it is a valid UUID.
    Uuid::parse_str(id).expect("should be valid UUID");
}

#[tokio::test]
async fn preserves_existing_request_id() {
    let plugin = RequestIdTransformPlugin;
    let headers = vec![("x-request-id".into(), "custom-id-123".into())];
    let mut ctx = make_request_ctx(headers);
    plugin.on_request(&mut ctx).await.unwrap();

    let val = ctx
        .headers
        .iter()
        .find(|(k, _)| k == "x-request-id")
        .map(|(_, v)| v.as_str());
    assert_eq!(val, Some("custom-id-123"));
}

#[tokio::test]
async fn preserves_existing_request_id_mixed_case() {
    let plugin = RequestIdTransformPlugin;
    let headers = vec![("X-Request-ID".into(), "mixed-case-id".into())];
    let mut ctx = make_request_ctx(headers);
    plugin.on_request(&mut ctx).await.unwrap();

    // Should NOT inject a duplicate — the mixed-case key should be detected.
    assert_eq!(ctx.headers.len(), 1, "should not inject duplicate header");
    let val = ctx
        .headers
        .iter()
        .find(|(k, _)| k == "X-Request-ID")
        .map(|(_, v)| v.as_str());
    assert_eq!(val, Some("mixed-case-id"));
}

#[tokio::test]
async fn on_response_default_is_noop() {
    let plugin = RequestIdTransformPlugin;
    let mut ctx = TransformResponseContext {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        config: HashMap::new(),
        security_context: test_security_context(),
    };
    let original_headers = ctx.headers.clone();
    plugin.on_response(&mut ctx).await.unwrap();
    assert_eq!(ctx.headers, original_headers);
}

//! Router-level tests for the erasure route. The security context the
//! gateway injects in production is injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_erasure_routes;
use crate::domain::erasure::ErasureService;
use crate::domain::subject_settings::SubjectKey;
use crate::test_support::{AllowResolver, DenyResolver, MemoryDeletionStore, context_of};

struct Subject {
    key: SubjectKey,
    store: Arc<MemoryDeletionStore>,
}

impl Subject {
    fn new() -> Self {
        Self {
            key: SubjectKey {
                tenant_id: Uuid::new_v4(),
                subject_id: Uuid::new_v4(),
            },
            store: Arc::default(),
        }
    }

    /// The route as `caller` of the subject's tenant sees it.
    fn routes(&self, resolver: Arc<dyn AuthZResolverApi>, caller: Uuid) -> Router {
        let service = ErasureService::new(
            Arc::clone(&self.store) as _,
            Arc::default() as Arc<crate::test_support::RecordingDeletionEvents>,
            PolicyEnforcer::new(resolver),
        );
        register_erasure_routes(
            Router::new(),
            &OpenApiRegistryImpl::new(),
            Arc::new(service),
        )
        .layer(Extension(context_of(caller, self.key.tenant_id)))
    }

    fn uri(&self) -> String {
        format!("/construct/v1/subjects/{}/erasure", self.key.subject_id)
    }
}

async fn send(router: Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_owner_is_answered_202_with_the_erasure_under_way_and_no_store() {
    let subject = Subject::new();

    let (status, headers, body) = send(
        subject.routes(Arc::new(AllowResolver), subject.key.subject_id),
        &subject.uri(),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        body,
        json!({
            "subject_id": subject.key.subject_id,
            "personalization_enabled": false,
            "erasure_in_progress": true,
        })
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert!(subject.store.is_marked(subject.key));
}

#[tokio::test]
async fn another_subject_or_a_refusal_answers_404_and_marks_nothing() {
    let subject = Subject::new();
    for router in [
        subject.routes(Arc::new(AllowResolver), Uuid::new_v4()),
        subject.routes(Arc::new(DenyResolver), subject.key.subject_id),
    ] {
        let (status, _, body) = send(router, &subject.uri()).await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(!subject.store.is_marked(subject.key));
    }
}

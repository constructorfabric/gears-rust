#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Router tests: oneshot requests against [`register_routes`] wired to a
//! fake [`PolicyManagementClientV1`] that returns canned entities and records
//! what it was asked.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use policy_engine_sdk::management::{
    Assignment, AssignmentSpec, Bundle, BundlePatch, BundleVersion, Document, DocumentSpec,
    ManagementError, NewBundle, PolicyManagementClientV1, ValidationFinding, ValidationReport,
    VersionContent, VersionDetail, VersionState,
};
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::resource_error;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use super::register_routes;
use crate::domain::management::test_support::ctx as make_ctx;

#[resource_error(gts_id!("cf.core.policy_engine.bundle.v1~"))]
struct FakeBundleError;

const BUNDLE: Uuid = Uuid::from_u128(0xB1);
const VERSION: Uuid = Uuid::from_u128(0xF1);
const ASSIGNMENT: Uuid = Uuid::from_u128(0xA5);
const TENANT: Uuid = Uuid::from_u128(0x7E_4A47);

fn at() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

fn bundle() -> Bundle {
    Bundle {
        id: BUNDLE,
        owner_tenant_id: TENANT,
        name: "b".to_owned(),
        description: String::new(),
        active_version_id: None,
        created_at: at(),
        created_by: TENANT,
        updated_at: at(),
    }
}

fn version(state: VersionState) -> BundleVersion {
    BundleVersion {
        id: VERSION,
        bundle_id: BUNDLE,
        owner_tenant_id: TENANT,
        ordinal: 1,
        state,
        created_at: at(),
        activated_at: None,
        activated_by: None,
    }
}

fn assignment(enforce: bool) -> Assignment {
    Assignment {
        id: ASSIGNMENT,
        bundle_id: BUNDLE,
        tenant_id: TENANT,
        owner_tenant_id: TENANT,
        enforce,
        created_at: at(),
        updated_at: at(),
    }
}

fn document() -> Document {
    Document {
        id: Uuid::from_u128(0xD0),
        spec: DocumentSpec {
            name: "main".to_owned(),
            content: "package policy\ndeny := false".to_owned(),
            resource_types: vec!["gts.cf.core.widget.v1~".to_owned()],
            actions: vec!["delete".to_owned()],
        },
    }
}

fn detail() -> VersionDetail {
    VersionDetail {
        version: version(VersionState::Draft),
        documents: vec![document()],
    }
}

/// Canned-answer double recording the last call it received.
#[derive(Default)]
struct FakeClient {
    calls: Mutex<Vec<String>>,
    missing: bool,
}

impl FakeClient {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }

    fn last(&self) -> String {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn refuse(&self) -> Result<(), ManagementError> {
        if self.missing {
            return Err(FakeBundleError::not_found("bundle not found")
                .with_resource(BUNDLE.to_string())
                .create());
        }
        Ok(())
    }
}

#[async_trait]
impl PolicyManagementClientV1 for FakeClient {
    async fn create_bundle(
        &self,
        _ctx: &SecurityContext,
        bundle: NewBundle,
    ) -> Result<Bundle, ManagementError> {
        self.record(format!(
            "create_bundle {} {}",
            bundle.name, bundle.description
        ));
        Ok(self::bundle())
    }

    async fn get_bundle(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Bundle, ManagementError> {
        self.refuse()?;
        Ok(bundle())
    }

    async fn list_bundles(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<Bundle>, ManagementError> {
        Ok(Page::new(
            vec![bundle()],
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        ))
    }

    async fn update_bundle(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        patch: BundlePatch,
    ) -> Result<Bundle, ManagementError> {
        self.record(format!(
            "update_bundle {:?} {:?}",
            patch.name, patch.description
        ));
        Ok(bundle())
    }

    async fn create_draft_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        seed_from: Option<Uuid>,
    ) -> Result<BundleVersion, ManagementError> {
        self.record(format!("create_draft_version {seed_from:?}"));
        Ok(version(VersionState::Draft))
    }

    async fn get_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        _version: Uuid,
    ) -> Result<VersionDetail, ManagementError> {
        Ok(detail())
    }

    async fn list_versions(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
    ) -> Result<Vec<BundleVersion>, ManagementError> {
        Ok(vec![version(VersionState::Active)])
    }

    async fn replace_draft_content(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        _version: Uuid,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError> {
        self.record(format!("replace_draft_content {:?}", content.documents));
        Ok(detail())
    }

    async fn delete_draft_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        _version: Uuid,
    ) -> Result<(), ManagementError> {
        self.record("delete_draft_version");
        Ok(())
    }

    async fn validate_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        version: Uuid,
    ) -> Result<ValidationReport, ManagementError> {
        Ok(ValidationReport {
            version_id: version,
            findings: vec![ValidationFinding {
                document_name: Some("main".to_owned()),
                code: "SYNTAX_ERROR".to_owned(),
                message: "bad".to_owned(),
            }],
            validated_at: at(),
        })
    }

    async fn activate_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: Uuid,
        _version: Uuid,
    ) -> Result<BundleVersion, ManagementError> {
        self.record("activate_version");
        Ok(version(VersionState::Active))
    }

    async fn assign(
        &self,
        _ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<Assignment, ManagementError> {
        self.record(format!("assign enforce={}", spec.enforce));
        Ok(assignment(spec.enforce))
    }

    async fn get_assignment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<Assignment, ManagementError> {
        Ok(assignment(true))
    }

    async fn update_assignment(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        enforce: bool,
    ) -> Result<Assignment, ManagementError> {
        self.record(format!("update_assignment enforce={enforce}"));
        Ok(assignment(enforce))
    }

    async fn unassign(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), ManagementError> {
        self.record("unassign");
        Ok(())
    }
}

// -- Harness ------------------------------------------------------------------

fn build_router_with(client: FakeClient) -> (Router, Arc<FakeClient>) {
    let client = Arc::new(client);
    let openapi = OpenApiRegistryImpl::new();
    let router = register_routes(
        Router::new(),
        &openapi,
        Arc::clone(&client) as Arc<dyn PolicyManagementClientV1>,
    );
    (router, client)
}

fn build_router() -> (Router, Arc<FakeClient>) {
    build_router_with(FakeClient::default())
}

fn request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    request_ctx(
        method,
        uri,
        body,
        Some(make_ctx(Uuid::from_u128(0xA11CE), TENANT)),
    )
}

fn request_ctx(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: Option<SecurityContext>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let bytes = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };
    let mut req = builder.body(bytes).unwrap();
    if let Some(ctx) = ctx {
        req.extensions_mut().insert(ctx);
    }
    req
}

async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(request(method, uri, body))
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn bundle_uri() -> String {
    format!("/policy-engine/v1/bundles/{BUNDLE}")
}

fn version_uri() -> String {
    format!("{}/versions/{VERSION}", bundle_uri())
}

fn assignment_uri() -> String {
    format!("/policy-engine/v1/assignments/{ASSIGNMENT}")
}

// -- Bundles ------------------------------------------------------------------

#[tokio::test]
async fn bundles_create_get_list_update() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/bundles",
        Some(serde_json::json!({ "name": "b", "description": "d" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        headers.get(header::LOCATION).unwrap().to_str().unwrap(),
        format!("/policy-engine/v1/bundles/{BUNDLE}")
    );
    assert!(headers.get(header::ETAG).is_none(), "no entity tags");
    assert!(body.get("etag").is_none());
    assert_eq!(client.last(), "create_bundle b d");

    let (status, headers, body) = send(&router, "GET", &bundle_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::ETAG).is_none());
    assert_eq!(body["name"], "b");

    let (status, _, body) = send(&router, "GET", "/policy-engine/v1/bundles", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["page_info"]["limit"].is_number());

    // A PATCH needs no precondition header.
    let (status, _, _) = send(
        &router,
        "PATCH",
        &bundle_uri(),
        Some(serde_json::json!({ "name": "n", "description": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(client.last(), "update_bundle Some(\"n\") Some(\"x\")");
}

#[tokio::test]
async fn a_missing_bundle_is_404() {
    let (router, _) = build_router_with(FakeClient {
        missing: true,
        ..FakeClient::default()
    });
    let (status, _, _) = send(&router, "GET", &bundle_uri(), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// -- Versions -----------------------------------------------------------------

#[tokio::test]
async fn versions_create_read_list_and_carry_content_only_where_documented() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        &format!("{}/versions", bundle_uri()),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(headers.get(header::LOCATION).is_some());
    assert_eq!(body["state"], "draft");
    assert_eq!(client.last(), "create_draft_version None");

    let (status, _, body) = send(&router, "GET", &version_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    let doc = &body["documents"][0];
    assert_eq!(doc["content"], "package policy\ndeny := false");
    assert_eq!(doc["resource_types"][0], "gts.cf.core.widget.v1~");
    assert_eq!(doc["actions"][0], "delete");
    assert!(doc.get("backend").is_none() && doc.get("targets").is_none());

    let (status, _, body) = send(&router, "GET", &format!("{}/versions", bundle_uri()), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["state"], "active");
    assert!(!body.to_string().contains("content"));
    assert!(!body.to_string().contains("digest"));
}

#[tokio::test]
async fn replace_validate_activate_and_delete_a_draft() {
    let (router, client) = build_router();

    let (status, _, body) = send(
        &router,
        "PUT",
        &version_uri(),
        Some(serde_json::json!({ "documents": [
            { "name": "main", "content": "package p\ndeny := true", "resource_types": ["gts.cf.core.*"] }
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["documents"].as_array().unwrap().len(), 1);
    assert!(
        client
            .last()
            .contains("resource_types: [\"gts.cf.core.*\"], actions: []")
    );

    let (status, _, body) = send(
        &router,
        "POST",
        &format!("{}/validate", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], false);
    assert_eq!(body["findings"][0]["code"], "SYNTAX_ERROR");
    assert!(body.get("etag").is_none());

    let (status, _, body) = send(
        &router,
        "POST",
        &format!("{}/activate", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "active");
    assert_eq!(client.last(), "activate_version");

    let (status, headers, _) = send(&router, "DELETE", &version_uri(), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.get(header::ETAG).is_none());
    assert_eq!(client.last(), "delete_draft_version");
}

#[tokio::test]
async fn there_is_no_deprecate_route() {
    let (router, _) = build_router();
    let (status, _, _) = send(
        &router,
        "POST",
        &format!("{}/deprecate", version_uri()),
        None,
    )
    .await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
        "{status}"
    );
}

// -- Assignments --------------------------------------------------------------

#[tokio::test]
async fn assignments_assign_get_update_unassign() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/assignments",
        Some(serde_json::json!({ "bundle_id": BUNDLE, "tenant_id": TENANT })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(headers.get(header::LOCATION).is_some());
    assert_eq!(body["enforce"], true, "enforcing by default");
    assert!(body.get("priority").is_none() && body.get("reaches_barriers").is_none());
    assert_eq!(client.last(), "assign enforce=true");

    let (_, _, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/assignments",
        Some(serde_json::json!({ "bundle_id": BUNDLE, "tenant_id": TENANT, "enforce": false })),
    )
    .await;
    assert_eq!(body["enforce"], false);

    let (status, _, body) = send(&router, "GET", &assignment_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], ASSIGNMENT.to_string());

    let (status, _, body) = send(
        &router,
        "PATCH",
        &assignment_uri(),
        Some(serde_json::json!({ "enforce": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enforce"], false);
    assert_eq!(client.last(), "update_assignment enforce=false");

    let (status, _, _) = send(&router, "DELETE", &assignment_uri(), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client.last(), "unassign");
}

// -- Authentication -----------------------------------------------------------

#[tokio::test]
async fn unauthenticated_and_anonymous_requests_are_refused() {
    let (router, _) = build_router();
    let resp = router
        .clone()
        .oneshot(request_ctx("GET", "/policy-engine/v1/bundles", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let anonymous = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();
    let resp = router
        .oneshot(request_ctx(
            "GET",
            "/policy-engine/v1/bundles",
            None,
            Some(anonymous),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
